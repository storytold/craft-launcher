# craft-launcher-android：把 ArtCraft Launcher 接到 Android

上游 `storytold/craft-launcher`（v0.4.1）没有移动端代码。这里同样是「加一个平台 crate」：
复用 `artcraft-launcher-ui-egui`（**真正的启动器界面**）和 `artcraft-launcher-native`
（**真正的下载/安装后端**），只补平台壳。

**关键点：本 crate 是新建的，`apps/artcraft-launcher` 一行都不用改。** 桌面壳的
`notify-rust` / `rfd` / `tray-icon` / `zbus` / `objc2` / `winreg` / `env_logger` 根本不会被引入，
所以不存在「把桌面依赖 cfg 掉」的脏活。

## 1. 放进仓库

```bash
cp -R ports/craft-launcher-android <craft-launcher-clone>/apps/craft-launcher-android
```

根 `Cargo.toml` 不用改（`members = ["crates/*", "apps/*", "xtask"]`）。

## 2. 应用上游补丁

```bash
cd <craft-launcher-clone>
git apply ~/artcraft-android/ports/craft-launcher-android/upstream/0001-release-owner-override.patch
```

一个补丁，三件事（详见 `docs/LAUNCHER-DOWNLOAD-URLS.md`）：

| 文件 | 改动 | 为什么 Android 需要 |
|---|---|---|
| `crates/core/src/catalog.rs` | 新增 `RELEASE_OWNER` + `CraftApp::release_repo()` | 下载地址指向我们自己的移植仓库 |
| `crates/native/src/backend.rs` | `feed_repo()` 改用 `release_repo()` | 上面那个常量的**唯一**生效点 |
| `crates/native/src/paths.rs` | 新增 `Paths::set_root()` | `dirs` 在 Android 返回 `None`，app 也写不了 `/tmp` |
| `crates/native/src/http.rs` | Android 从 `/system/etc/security/cacerts` 读根证书 | `rustls-platform-verifier` 在 Android 需要额外的 Kotlin 组件 + Maven 依赖（`org.rustls:rustls-platform-verifier-android`），移植版没法提供 |
| `crates/ui-egui/src/cjk_font.rs` | 中文候选加入 `/system/fonts` | 否则中文界面渲染成方块 |

## 3. 构建

```bash
cd ~/artcraft-android
scripts/build-so.sh craft-launcher          # → apps/craft-launcher-android/jniLibs/arm64-v8a/libmain.so
scripts/make-shell.sh "$PWD/src/craft-launcher" craft-launcher-android \
    ai.storyteller.artcraft-launcher "ArtCraft Launcher" 0.4.1 craft-launcher
scripts/build-apk.sh craft-launcher
```

`build-so.sh` 从 `ports/<pkg>/Cargo.toml` 读 `[package] name`，因为**目录名和 crate 名不一样**：
目录是 `apps/craft-launcher-android`，crate 是 `artcraft-launcher-android`。`cargo ndk -p` 要的是
crate 名。

**Android 包名与上游 bundle id 不同**：上游是 `ai.storyteller.artcraft-launcher`，而连字符在
Android 的 `applicationId` / Java 包里**不合法**（AGP 直接报
`Namespace ... is not a valid Java package name as 'artcraft-launcher' is not a valid Java identifier`）。
所以 Android 包名去掉连字符：**`ai.storyteller.artcraftlauncher`**。这是唯一一处移植版和上游标识
不一样的地方，`scripts/env.sh` 的 `artcraft_app_id()` 里记着。

> **注意构建时间**：上游 `[profile.release]` 是 `lto = "thin"` + `codegen-units = 1`。桌面发布二进制
> 这样是对的，但在这个依赖图上会很慢。要快可以：
> `CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16`（产物更大）。

## 4. 这个 crate 做了什么

| 桌面（`apps/artcraft-launcher`） | Android（本 crate） |
|---|---|
| 托盘图标、「关闭到托盘」、`tray-icon` | 无托盘：关窗即退出 |
| `notify-rust` 系统通知 | 写进 logcat（接 `NotificationManager` 需要 JNI 桥，后续再做） |
| `rfd` 文件夹选择器 | `None`：安装位置固定为 app 私有目录 |
| `dirs` 的 data/cache 目录 | `AndroidApp::internal_data_path()`，在 backend 启动**之前**注入 |
| 登录项、macOS Spaces、Linux 主题 portal | 不适用 |
| `single_instance`（loopback TCP 47413） | 原样保留，Android 沙盒内可用 |
| `env_logger` | `android_logger` |
| 桌面尺寸（1120×760） | 同一尺寸；`with_decorations(false)` 的头部即标题栏 |

`Host` trait 的默认值正好就是 Android 的答案（无托盘、无桌面、无登录项），所以
`AndroidHost` 只覆写 `title_bar()` / `notify()` / `quit()` 三个。

关窗时若 `ui.busy()`（正在下载/安装）会 `CancelClose`——桌面壳靠托盘保住窗口，Android 没有
托盘，关掉就杀进程，会把装到一半的东西留下。

## 5. 编译验证（已实测）

```bash
# 两个上游 crate 都能编到 aarch64-linux-android
cargo +stable ndk -t arm64-v8a check -p artcraft-launcher-ui-egui     # 0 error
cargo +stable ndk -t arm64-v8a check -p artcraft-launcher-native      # 0 error（2 个 dead-code warning）
cargo +stable ndk -t arm64-v8a check --tests -p artcraft-launcher-native   # Finished, 0 error

# 下载地址逻辑
cargo +stable test -p artcraft-launcher-core --lib    # 34 passed; 0 failed
```

macOS 上 `cargo +stable check --tests -p artcraft-launcher-native` 会失败（`self_update.rs` 的
`install_dmg` 参数个数，E0061）——这是**上游既有问题**，与本次改动无关，已在 pristine HEAD 上用
`git stash` 确认。Android target 上那些 macOS-only 的测试代码被 cfg 掉，所以全绿。

## 6. 已知缺口

- **通知**：只有 logcat，没有真的系统通知。
- **文件夹选择**：`pick_folder` 是 `None`；`install_locations.root` 这类设置改不了。
  要真可用得写 SAF（`ACTION_OPEN_DOCUMENT_TREE`）。
- **安装语义**：`NativeBackend` 装的是**桌面包**（`.dmg`/`.msi`/`.AppImage`/`.deb`）。
  在 Android 上它只应该装**我们自己的 APK release**。所以移植仓库里每个 release 的资产名必须
  能让 `platform::reads_asset_names` 认出来，否则会显示「没有适合这个平台的包」。
  这一层（Android 的 PackageKind + APK 安装走 `PackageInstaller` Intent）**还没做**。
- **证书校验**：读系统 CA 目录，放弃了吊销检查（CRL/OCSP）和用户安装的根证书。对 GitHub 的
  证书没影响，对企业 MITM 代理有影响。
- **触屏 UI**：和 GridCraft 一样，界面是按桌面设计的；网格卡片在大屏手机上能用，但没有专门的
  触控布局。

## 7. 2026-10-24 修复：已安装包名与桌面 feed 缓存（真机验证时发现）

两处缺陷让启动器与手机、与移植仓库自己的 release 不一致：

1. **PdfCraft 永远显示「可安装」。** Android 的 applicationId 一旦发布就不能改，PdfCraft 的
   移植包一直是 `ai.storyteller.printcraft`，而 `detect`/`launch` 都按目录 id 拼成
   `ai.storyteller.pdfcraft`。于是即使 APK 已装，卡片状态仍是 `可安装`，`打开` 也会去启动一个
   不存在的包。修复：`CraftApp::android_bundle_id()` 持有这一处别名
   （`ANDROID_PACKAGE_IDS = [("pdfcraft", "ai.storyteller.printcraft")]`），Android 的检测与
   启动都改用它；桌面路径不变。
2. **`ARTCRAFT_LAUNCHER_NO_FEED` 没有绕过缓存。** 网络请求确实跳过了，但
   `feed_and_fallback` 仍会 `load_feed` 读上次存下的 `feed.json`，那是上游**桌面** release
   的数据：每张卡都显示 0.15.x / 0.16.x（本平台没有对应包），PdfCraft 显示「此平台不可用」，
   启动器自己还提示可更新到 0.7.0（同样没有 Android APK）。修复：feed 被禁用时连缓存一起
   跳过，只信 `RELEASE_REPO` 的 GitHub 回答。

3. **全新安装后的第一次启动，所有卡片都显示「可安装」。** `MainActivity` 在
   `super.onCreate()` 之后才把 `installed-apps.txt` 写给 Rust；而 GameActivity 正是在
   `super.onCreate()` 里启动原生线程，Rust 只读一次这个文件。首次安装时还没有上一次留下的
   文件，于是检测先跑、文件后到，13 个已安装应用全被读成未安装；重启一次又正常。修复：把
   `writeInstalledApps()` 提到 `super.onCreate()` 之前（Activity 的 context 此时已 attach，
   `PackageManager`/`getFilesDir()` 可用）。

同时 `versionName` 从 0.4.1 对到 0.7.0（crate 的 workspace 版本）。

真机验证（vivo PA2573）：卸载旧包 → 安装 → 第一次冷启动，13 张卡全部 `已安装` 且按钮为
`打开`，`files/installed-apps.txt` 与 `pm list packages` 一致。

**代码在 fork 分支，不在本目录：** 上游 crate 的改动提交在
`github.com/tzhazuma/craft-launcher` 的 `android-port` 分支（`bcaded0`，父提交 `312c0f8`），
`~/artcraft-android` 只记录 gitlink。本目录下的两个 patch 文件（`upstream/0001-...` 与
`~/artcraft-android/patches/launcher-platform-android.patch`）是当时的合并快照，已经**不能**
apply 到 `android-port` 的当前树；重建时以 fork 分支为准。
