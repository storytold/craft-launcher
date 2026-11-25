use super::*;
use artcraft_launcher_core::{Arch, Os};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::cell::Cell;

const NOW: i64 = 1_795_190_400;
fn app() -> &'static CraftApp {
    CraftApp::by_id("photocraft").unwrap()
}
fn platform() -> Platform {
    Platform { os: Os::Linux, arch: Arch::X64 }
}
fn tag(run: u64) -> String {
    format!("v1.0.0-nightly.20261120.{run}.1.gabcdefabcdef")
}
fn api(page: usize) -> String {
    if page == 1 {
        format!("https://api.github.com/repos/{}/releases?per_page=100", app().repo)
    } else {
        format!("https://api.github.com/repos/{}/releases?per_page=100&page={page}", app().repo)
    }
}

/// Entirely synthetic metadata/body fixture; its payload is never downloaded or executed.
pub(crate) fn candidate(tag: &str, linux: bool) -> (Value, String, Vec<u8>) {
    let version = version::parse(tag).unwrap();
    let (os, arch, format, suffix) = if linux {
        ("linux", "x86_64", "tar_gz", "linux-x86_64-nightly.tar.gz")
    } else {
        ("macos", "aarch64", "portable_zip", "macos-aarch64-nightly.zip")
    };
    let name = format!("photocraft-{version}-{suffix}");
    let prefix = format!("https://github.com/{}/releases/download/{tag}/", app().repo);
    let mut value = json!({"schema":"craft-nightly/v2","channel":"nightly","app":"photocraft","repository":app().repo,
        "tag":tag,"source_commit":"abcdefabcdefabcdefabcdefabcdefabcdefabcd","published_at":NOW,
        "artifacts":[{"target":{"os":os,"arch":arch,"format":format},"asset":{"name":name,"url":format!("{prefix}{name}"),"size":123,"sha256":"a".repeat(64)},
            "isolation":{"package_identity":"ai.storyteller.photocraft.nightly","profile_contract":"nightly","profile_version":1,"associations":"none"}}]});
    let signature_name = format!("{name}.asc");
    if linux {
        value["artifacts"][0]["signature"] =
            json!({"name":signature_name,"url":format!("{prefix}{signature_name}"),"size":123,"sha256":"b".repeat(64)});
    }
    let body = serde_json::to_vec(&value).unwrap();
    let url = format!("{prefix}NIGHTLY.json");
    let mut item = json!({"draft":false,"prerelease":true,"tag_name":tag,"published_at":"2026-11-20T16:00:00Z","assets":[
        {"name":name,"browser_download_url":format!("{prefix}{name}"),"size":123,"digest":format!("sha256:{}","a".repeat(64))},
        {"name":"NIGHTLY.json","browser_download_url":url,"size":body.len(),"digest":format!("sha256:{:x}",Sha256::digest(&body))}]});
    if linux {
        item["assets"].as_array_mut().unwrap().push(json!({"name":signature_name,"browser_download_url":format!("{prefix}{signature_name}"),"size":123,"digest":format!("sha256:{}","b".repeat(64))}));
    }
    (item, url, body)
}
fn ordinary(index: usize) -> Value {
    json!({"draft":false,"prerelease":false,"tag_name":format!("v9.0.{index}"),"assets":[]})
}
fn fixtures(items: Vec<Value>, resources: Vec<(String, Vec<u8>)>) -> BTreeMap<String, Vec<u8>> {
    let mut data: BTreeMap<_, _> = resources.into_iter().collect();
    data.insert(api(1), serde_json::to_vec(&items).unwrap());
    data
}
fn ask(data: &BTreeMap<String, Vec<u8>>) -> Outcome {
    lookup(app(), platform(), NOW, &mut |url, _, _, _| Ok(data.get(url).cloned()), &|| Duration::ZERO, &|| true)
}
pub(crate) fn verified(run: u64) -> VerifiedRelease {
    let (item, _, body) = candidate(&tag(run), true);
    let release = artcraft_launcher_core::release::parse_github_release(&item.to_string(), app().repo).unwrap();
    crate::nightly::verified_descriptor(app(), release, &body, platform()).unwrap().unwrap()
}

#[test]
fn complete_lookup_uses_semver_and_checks_compatible_coverage_not_publication_order() {
    let (mut newer, nu, nb) = candidate(&tag(20), true);
    let (mut older, ou, ob) = candidate(&tag(10), true);
    newer["published_at"] = json!("2026-11-20T16:00:00Z");
    older["published_at"] = json!("2026-11-21T16:00:00Z");
    let (unsupported, uu, ub) = candidate(&tag(30), false);
    let data = fixtures(vec![older, newer, unsupported], vec![(nu, nb), (ou, ob), (uu, ub)]);
    let Outcome::Verified(selected) = ask(&data) else { panic!("complete compatible lookup should select") };
    assert_eq!(selected.release().tag, tag(20));
}

#[test]
fn valid_unsupported_descriptor_is_authoritative_but_invalid_higher_is_indeterminate() {
    let (unsupported, url, body) = candidate(&tag(30), false);
    assert!(matches!(ask(&fixtures(vec![unsupported], vec![(url, body)])), Outcome::NoCompatibleBuild));
    let (good, gu, gb) = candidate(&tag(10), true);
    let (bad, bu, mut bb) = candidate(&tag(20), true);
    bb.push(b' ');
    assert!(matches!(ask(&fixtures(vec![good, bad], vec![(gu, gb), (bu, bb)])), Outcome::Indeterminate(_)));
    let invalid = json!({"draft":false,"prerelease":true,"tag_name":"v1.0.0-nightly.20261120.001.1.gabcdefabcdef","assets":[]});
    assert!(matches!(ask(&fixtures(vec![invalid], vec![])), Outcome::Indeterminate(_)));
    assert!(matches!(ask(&fixtures(vec![], vec![])), Outcome::NoCompatibleBuild));
}

#[test]
fn mixed_full_pages_continue_until_complete_and_budget_exhaustion_discards_seen_candidate() {
    let (item, url, body) = candidate(&tag(20), true);
    let mut data = fixtures((0..100).map(ordinary).collect(), vec![(url.clone(), body.clone())]);
    data.insert(api(2), serde_json::to_vec(&vec![item.clone()]).unwrap());
    assert!(matches!(ask(&data), Outcome::Verified(_)));
    let mut first: Vec<_> = (0..99).map(ordinary).collect();
    first.push(item);
    data.insert(api(1), serde_json::to_vec(&first).unwrap());
    for page in 2..=MAX_PAGES {
        data.insert(api(page), serde_json::to_vec(&(0..100).map(|i| ordinary(i + page * 100)).collect::<Vec<_>>()).unwrap());
    }
    let mut requests = 0;
    let outcome = lookup(
        app(),
        platform(),
        NOW,
        &mut |url, _, _, _| {
            requests += 1;
            Ok(data.get(url).cloned())
        },
        &|| Duration::ZERO,
        &|| true,
    );
    assert!(matches!(outcome, Outcome::Indeterminate(_)));
    assert_eq!(requests, MAX_PAGES, "all bounded pages, no unseen sixth page or premature descriptor fetch");
}

#[test]
fn descriptor_request_budget_is_indeterminate_before_a_compatible_winner() {
    let mut items = Vec::new();
    let mut resources = Vec::new();
    for run in 1..=MAX_REQUESTS + 2 {
        let (item, url, body) = candidate(&tag(run as u64), false);
        items.push(item);
        resources.push((url, body));
    }
    let data = fixtures(items, resources);
    let mut requests = 0;
    let outcome = lookup(
        app(),
        platform(),
        NOW,
        &mut |url, _, _, _| {
            requests += 1;
            Ok(data.get(url).cloned())
        },
        &|| Duration::ZERO,
        &|| true,
    );
    assert!(matches!(outcome, Outcome::Indeterminate(_)));
    assert_eq!(requests, MAX_REQUESTS);
}

#[test]
fn elapsed_and_returned_byte_limits_discard_partial_answers() {
    let elapsed = Cell::new(Duration::ZERO);
    let mut calls = 0;
    let result = lookup(
        app(),
        platform(),
        NOW,
        &mut |_, _, remaining, _| {
            assert!(remaining <= MAX_TIME);
            calls += 1;
            elapsed.set(MAX_TIME);
            Ok(Some(b"[]".to_vec()))
        },
        &|| elapsed.get(),
        &|| true,
    );
    assert!(matches!(result, Outcome::Indeterminate(_)));
    assert_eq!(calls, 1);
    let result = lookup(app(), platform(), NOW, &mut |_, max, _, _| Ok(Some(vec![b' '; max + 1])), &|| Duration::ZERO, &|| true);
    assert!(matches!(result, Outcome::Indeterminate(_)));
}

#[test]
fn cumulative_returned_byte_budget_is_indeterminate_with_candidate_metadata_and_incomplete_coverage() {
    let (item, descriptor_url, _) = candidate(&tag(20), true);
    let mut first: Vec<_> = (0..99).map(ordinary).collect();
    first.push(item);
    let remainder = 1024;
    let mut calls = 0;
    let mut returned_bytes = 0;
    let mut caps = Vec::new();
    let outcome = lookup(
        app(),
        platform(),
        NOW,
        &mut |url, max, _, _| {
            calls += 1;
            assert_ne!(url, descriptor_url, "incomplete page coverage must not fetch a descriptor");
            assert_eq!(url, api(calls), "all requests must cover release pages in order");
            caps.push(max);
            if calls == MAX_PAGES {
                assert_eq!(max, remainder, "the fifth page receives the positive remaining total budget");
                assert_eq!(returned_bytes, MAX_BYTES - remainder);
                return Err(failure("The remaining total byte budget cannot cover this page."));
            }
            let items = if url == api(1) { first.clone() } else { (0..100).map(ordinary).collect() };
            let mut body = serde_json::to_vec(&items).unwrap();
            body.resize(if calls == 1 { MAX_PAGE_BYTES - remainder } else { MAX_PAGE_BYTES }, b' ');
            returned_bytes += body.len();
            Ok(Some(body))
        },
        &|| Duration::ZERO,
        &|| true,
    );
    assert!(matches!(outcome, Outcome::Indeterminate(_)));
    assert_eq!(calls, MAX_PAGES);
    assert_eq!(returned_bytes, MAX_BYTES - remainder);
    assert!(caps[..MAX_PAGES - 1].iter().all(|max| *max == MAX_PAGE_BYTES));
    assert_eq!(caps.last(), Some(&remainder));
}

#[test]
fn fresh_off_and_off_during_a_response_make_no_followup_request_or_selection() {
    let mut calls = 0;
    let off = lookup(
        app(),
        platform(),
        NOW,
        &mut |_, _, _, _| {
            calls += 1;
            Ok(Some(b"[]".to_vec()))
        },
        &|| Duration::ZERO,
        &|| false,
    );
    assert!(matches!(off, Outcome::Indeterminate(_)));
    assert_eq!(calls, 0);
    let enabled = Cell::new(true);
    let (item, _, _) = candidate(&tag(20), true);
    let mut calls = 0;
    let later_off = lookup(
        app(),
        platform(),
        NOW,
        &mut |_, _, _, _| {
            calls += 1;
            enabled.set(false);
            Ok(Some(serde_json::to_vec(&vec![item.clone()]).unwrap()))
        },
        &|| Duration::ZERO,
        &|| enabled.get(),
    );
    assert!(matches!(later_off, Outcome::Indeterminate(_)));
    assert_eq!(calls, 1);
}

#[test]
fn metadata_keeps_attempt_and_verified_success_distinct_without_body_or_token_cache() {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::under(&temp.path().canonicalize().unwrap()).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
    let key = key(app(), platform());
    let mut state = State::default();
    assert_eq!(state.begin(std::slice::from_ref(&key), NOW, false, true).as_slice(), std::slice::from_ref(&key));
    state.persist_outcome(&paths, &key, NOW, &Outcome::Verified(Box::new(verified(20)))).unwrap();
    state.finish();
    assert!(state.fresh(&key, NOW));
    let later = NOW + EVERY;
    assert!(!state.begin(std::slice::from_ref(&key), later, false, true).is_empty());
    let error = Outcome::Indeterminate(NightlyHttpError { why: "offline".into(), retry_at: Some(later + 120) });
    let record = state.persist_outcome(&paths, &key, later, &error).unwrap();
    state.finish();
    assert_eq!(record.attempted_at, Some(later));
    assert_eq!(record.verified_at, Some(NOW));
    assert!(record.status(false).stale);
    assert_eq!(record.tag, Some(tag(20)));
    assert!(!state.fresh(&key, later));
    let restored = State::load(&paths);
    assert_eq!(restored.record(&key).verified_at, Some(NOW));
    assert!(restored.record(&key).status(false).stale);
    let disk = std::fs::read_to_string(paths.nightly_checks()).unwrap();
    assert!(!disk.contains("assets"));
    assert!(!disk.contains("source_commit"));
    assert!(!paths.release_cache().exists());
}

#[test]
fn six_hour_boundaries_manual_coalescing_and_clock_rollback_are_explicit() {
    let key = key(app(), platform());
    let mut state = State::default();
    assert!(state.begin(std::slice::from_ref(&key), NOW, false, false).is_empty());
    assert!(!state.begin(std::slice::from_ref(&key), NOW, false, true).is_empty());
    state.finish();
    assert!(state.begin(std::slice::from_ref(&key), NOW + EVERY - 1, false, true).is_empty());
    assert!(state.begin(std::slice::from_ref(&key), NOW + 59, true, true).is_empty());
    assert!(!state.begin(std::slice::from_ref(&key), NOW + 60, true, true).is_empty());
    state.finish();
    assert!(!state.begin(std::slice::from_ref(&key), NOW + 60 + EVERY, false, true).is_empty());
    state.finish();
    assert!(!state.begin(std::slice::from_ref(&key), NOW - 60, false, true).is_empty(), "future attempts are stale after rollback");
    state.finish();
    state.outcome(&key, NOW, &Outcome::Indeterminate(NightlyHttpError { why: "limited".into(), retry_at: Some(NOW + 120) }));
    assert!(state.begin(std::slice::from_ref(&key), NOW - 120, true, true).is_empty(), "rollback never bypasses an absolute server backoff");
    assert!(state.begin(std::slice::from_ref(&key), NOW + 119, true, true).is_empty());
    assert!(!state.begin(std::slice::from_ref(&key), NOW + 120, true, true).is_empty());
}

#[test]
fn simultaneous_manual_calls_share_one_inflight_lease() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(State::default()));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
    let handles: Vec<_> = (0..12)
        .map(|_| {
            let state = state.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                !state.lock().unwrap().begin(&[key(app(), platform())], NOW, true, true).is_empty()
            })
        })
        .collect();
    let starts = handles.into_iter().map(|thread| usize::from(thread.join().unwrap())).sum::<usize>();
    assert_eq!(starts, 1);
}

#[cfg(unix)]
#[test]
fn scheduling_write_refuses_shared_or_aliased_files_without_touching_stable_data() {
    let temp = tempfile::tempdir().unwrap();
    let stable = Paths::under(&temp.path().canonicalize().unwrap());
    std::fs::create_dir_all(&stable.data).unwrap();
    std::fs::write(stable.manifest(), b"stable sentinel").unwrap();
    let paths = stable.clone().for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
    std::fs::create_dir_all(&paths.data).unwrap();
    std::fs::hard_link(stable.manifest(), paths.nightly_checks()).unwrap();
    let state = State::default();
    assert!(state.save(&paths).is_err());
    assert_eq!(std::fs::read(stable.manifest()).unwrap(), b"stable sentinel");
    std::fs::remove_file(paths.nightly_checks()).unwrap();
    std::os::unix::fs::symlink(stable.manifest(), paths.nightly_checks()).unwrap();
    assert!(state.save(&paths).is_err());
    assert_eq!(std::fs::read(stable.manifest()).unwrap(), b"stable sentinel");
}

#[test]
fn final_coverage_checks_deadline_after_the_last_descriptor_validation_boundary() {
    let (item, url, body) = candidate(&tag(20), true);
    let data = fixtures(vec![item], vec![(url.clone(), body)]);
    let elapsed = Cell::new(Duration::ZERO);
    let final_response = Cell::new(false);
    let outcome = lookup(
        app(),
        platform(),
        NOW,
        &mut |asked, _, _, _| {
            if asked == url {
                final_response.set(true);
            }
            Ok(data.get(asked).cloned())
        },
        &|| elapsed.get(),
        &|| {
            if final_response.replace(false) {
                elapsed.set(MAX_TIME);
            }
            true
        },
    );
    assert!(matches!(outcome, Outcome::Indeterminate(_)));
}

#[test]
fn unknown_publication_or_channel_flags_never_become_authoritative_absence() {
    let unknown = json!({"tag_name":"v1.0.0-nightly.20261120.20.1.gabcdefabcdef","assets":[]});
    assert!(matches!(ask(&fixtures(vec![unknown], vec![])), Outcome::Indeterminate(_)));
}

/// Authored normal producer fixture: checksums plus isolation descriptor are distinct GETs.
fn producer_candidate(tag: &str, linux: bool) -> (Value, Vec<(String, Vec<u8>)>) {
    let (mut item, descriptor_url, body) = candidate(tag, linux);
    let assets = item["assets"].as_array_mut().unwrap();
    let sums: String = assets
        .iter()
        .map(|asset| format!("{}  {}\n", asset["digest"].as_str().unwrap().strip_prefix("sha256:").unwrap(), asset["name"].as_str().unwrap()))
        .collect();
    let sums_url = format!("https://github.com/{}/releases/download/{tag}/SHA256SUMS.txt", app().repo);
    assets.push(json!({"name":"SHA256SUMS.txt","browser_download_url":sums_url,"size":sums.len()}));
    (item, vec![(descriptor_url, body), (sums_url, sums.into_bytes())])
}

#[test]
fn retained_two_resource_history_is_order_independent_and_only_verifies_the_winner() {
    for reverse in [false, true] {
        let mut items = Vec::new();
        let mut resources = Vec::new();
        for run in 1..=20 {
            let (item, files) = producer_candidate(&tag(run), true);
            items.push(item);
            resources.extend(files);
        }
        if reverse {
            items.reverse();
        }
        let data = fixtures(items, resources);
        let mut asked = Vec::new();
        let outcome = lookup(
            app(),
            platform(),
            NOW,
            &mut |url, _, _, _| {
                asked.push(url.to_owned());
                Ok(data.get(url).cloned())
            },
            &|| Duration::ZERO,
            &|| true,
        );
        let Outcome::Verified(winner) = outcome else { panic!("normal retained history must remain discoverable") };
        assert_eq!(winner.release().tag, tag(20));
        assert_eq!(asked.len(), 3, "one complete page and the winning checksums/descriptor, not every older body");
        assert!(asked[1..].iter().all(|url| url.contains(&format!("/{}/", tag(20)))));
    }
}

#[test]
fn higher_unsupported_contenders_are_verified_before_the_compatible_winner() {
    let (lower, lower_files) = producer_candidate(&tag(10), true);
    let (winner, winner_files) = producer_candidate(&tag(20), true);
    let (higher, higher_files) = producer_candidate(&tag(30), false);
    let data = fixtures(vec![lower, higher, winner], lower_files.into_iter().chain(winner_files).chain(higher_files).collect());
    let mut asked = Vec::new();
    let outcome = lookup(
        app(),
        platform(),
        NOW,
        &mut |url, _, _, _| {
            asked.push(url.to_owned());
            Ok(data.get(url).cloned())
        },
        &|| Duration::ZERO,
        &|| true,
    );
    let Outcome::Verified(winner) = outcome else { panic!("valid unsupported row cannot hide a compatible lower version") };
    assert_eq!(winner.release().tag, tag(20));
    assert_eq!(asked.len(), 5);
    assert!(!asked.iter().any(|url| url.contains(&format!("/{}/", tag(10)))));
    assert!(asked[1].contains(&tag(30)));
    assert!(asked[3].contains(&tag(20)));
}

#[test]
fn invalid_potential_winner_refuses_but_dominated_descriptor_body_is_unnecessary() {
    for invalid_is_higher in [false, true] {
        let (valid, vu, vb) = candidate(&tag(20), true);
        let bad_tag = tag(if invalid_is_higher { 30 } else { 10 });
        let (bad, bu, mut bb) = candidate(&bad_tag, true);
        bb.push(b' ');
        let data = fixtures(vec![bad, valid], vec![(vu, vb), (bu.clone(), bb)]);
        let mut asked = Vec::new();
        let outcome = lookup(
            app(),
            platform(),
            NOW,
            &mut |url, _, _, _| {
                asked.push(url.to_owned());
                Ok(data.get(url).cloned())
            },
            &|| Duration::ZERO,
            &|| true,
        );
        if invalid_is_higher {
            assert!(matches!(outcome, Outcome::Indeterminate(_)));
            assert!(asked.contains(&bu));
        } else {
            let Outcome::Verified(winner) = outcome else { panic!("dominated invalid body is not winner evidence") };
            assert_eq!(winner.release().tag, tag(20));
            assert!(!asked.contains(&bu));
        }
    }
}

#[test]
fn equal_semver_build_metadata_and_optional_v_use_the_same_lexical_identity_tie() {
    let canonical = tag(20);
    let bare = canonical.strip_prefix('v').unwrap().to_owned();
    for (left, right) in [(canonical.clone(), bare), (format!("{canonical}+zzz"), format!("{canonical}+aaa"))] {
        for reverse in [false, true] {
            let (a, au, ab) = candidate(&left, true);
            let (b, bu, bb) = candidate(&right, true);
            let mut items = vec![a, b];
            if reverse {
                items.reverse();
            }
            let data = fixtures(items, vec![(au, ab), (bu, bb)]);
            let outcome = ask(&data);
            let Outcome::Verified(winner) = outcome else { panic!("equal-precedence canonical identities must remain verifiable; got {outcome:?}") };
            assert_eq!(winner.release().tag, left.clone().min(right.clone()));
        }
    }
}

#[test]
fn malformed_existing_metadata_is_not_fresh_absence_and_requires_valid_reload() {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::under(&temp.path().canonicalize().unwrap()).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
    let row = key(app(), platform());
    let mut fresh = State::load(&paths);
    assert!(!fresh.begin(std::slice::from_ref(&row), NOW, false, true).is_empty());
    fresh.save(&paths).unwrap();
    fresh.finish();
    let valid = std::fs::read(paths.nightly_checks()).unwrap();
    std::fs::write(paths.nightly_checks(), b"malformed existing metadata").unwrap();
    let mut blocked = State::load(&paths);
    assert!(blocked.record(&row).message.is_some());
    assert!(blocked.begin(std::slice::from_ref(&row), NOW + EVERY, true, true).is_empty());
    assert!(blocked.save(&paths).is_err());
    assert!(blocked.prepare(&paths).is_err());
    assert_eq!(std::fs::read(paths.nightly_checks()).unwrap(), b"malformed existing metadata");
    std::fs::remove_file(paths.nightly_checks()).unwrap();
    assert!(blocked.prepare(&paths).is_err(), "absence after an error is not a reset");
    std::fs::write(paths.nightly_checks(), valid).unwrap();
    blocked.prepare(&paths).unwrap();
    assert!(blocked.record(&row).message.is_none());
    assert!(!blocked.begin(std::slice::from_ref(&row), NOW + EVERY, true, true).is_empty());
}

#[test]
fn review_probe_ordinary_release_tags_do_not_poison_nightly_lookup() {
    let (nightly, url, body) = candidate(&tag(20), true);
    let ordinary = json!({"draft":false,"prerelease":false,"tag_name":"release/2026","assets":[]});
    let data = fixtures(vec![ordinary, nightly], vec![(url, body)]);
    let outcome = ask(&data);
    assert!(matches!(outcome, Outcome::Verified(_)), "an explicitly ordinary tag must not poison compatible Nightly coverage");
    for uncertain in
        [json!({"draft":false,"tag_name":"release/2026","assets":[]}), json!({"draft":false,"prerelease":true,"tag_name":"release/2026","assets":[]})]
    {
        assert!(
            matches!(ask(&fixtures(vec![uncertain], vec![])), Outcome::Indeterminate(_)),
            "a missing flag or malformed potential pre-release remains indeterminate"
        );
    }
}

#[test]
fn shared_selection_core_and_native_rank_one_mixed_canonical_list_identically() {
    use artcraft_launcher_core::protocol::VersionTrack;
    let high = tag(20);
    let bare = high.strip_prefix('v').unwrap();
    let expected = format!("{bare}+aaa");
    let tags = [tag(10), format!("{high}+zzz"), format!("{high}+aaa"), format!("{bare}+zzz"), expected.clone()];
    let mut items = Vec::new();
    let mut winner_body = None;
    for (index, tag) in tags.iter().enumerate() {
        let (mut item, url, body) = candidate(tag, true);
        item["published_at"] = json!(if index == 0 { "2026-11-21T16:00:00Z" } else { "2026-11-20T16:00:00Z" });
        if tag == &expected {
            winner_body = Some((url, body));
        }
        items.push(item);
    }
    items.push(ordinary(0));
    items.push(json!({"draft":false,"prerelease":true,"tag_name":"v9.0.0-beta.1","assets":[]}));
    for rotation in 0..3 {
        let mut permutation = items.clone();
        permutation.rotate_left(rotation);
        if rotation == 1 {
            permutation.reverse();
        }
        let core = artcraft_launcher_core::release::parse_github_versions(&serde_json::to_string(&permutation).unwrap(), app().repo).unwrap();
        let selected = artcraft_launcher_core::release::choose_version(&core, &VersionTrack::Nightly).unwrap();
        assert_eq!(selected.tag, expected);
        // Only the winner's body exists: dominated descriptor bodies are not newly fetched.
        let data = fixtures(permutation, vec![winner_body.clone().unwrap()]);
        let Outcome::Verified(native) = ask(&data) else { panic!("canonical mixed-list lookup must verify the shared core winner") };
        assert_eq!(native.release().tag, selected.tag);
    }
}
#[test]
fn shared_selection_native_missing_winning_evidence_is_not_an_ordinary_checksum_filter() {
    use artcraft_launcher_core::protocol::VersionTrack;
    let (low, url, body) = candidate(&tag(10), true);
    let (mut high, _, _) = candidate(&tag(20), true);
    high["assets"] = json!([]);
    let items = vec![low, high];
    let core = artcraft_launcher_core::release::parse_github_versions(&serde_json::to_string(&items).unwrap(), app().repo).unwrap();
    assert_eq!(artcraft_launcher_core::release::choose_version(&core, &VersionTrack::Nightly).unwrap().tag, tag(10));
    assert!(
        matches!(ask(&fixtures(items, vec![(url, body)])), Outcome::Indeterminate(_)),
        "the native channel must not silently turn missing winning evidence into a lower verified selection"
    );
}
#[test]
fn shared_selection_unconfigured_platform_lookup_refuses_before_any_network_body() {
    let mut requests = 0;
    let result = lookup(
        app(),
        // Intel Macs have no Nightly row; Apple silicon is admitted by its pinned signing team.
        Platform { os: Os::MacOs, arch: Arch::X64 },
        NOW,
        &mut |_, _, _, _| {
            requests += 1;
            Ok(None)
        },
        &|| Duration::ZERO,
        &|| true,
    );
    assert!(matches!(result, Outcome::Indeterminate(_)));
    assert_eq!(requests, 0);
}
