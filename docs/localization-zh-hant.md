# 繁體中文（台灣）介面 / Traditional Chinese (Taiwan) interface

更新日期 / Updated: 2026-11-03

## 繁體中文

在 **設定 → 語言** 選擇「繁體中文」，或把介面語言設為 `zh-TW`。設定會保存，並在下次啟動時
沿用。首次啟動時，系統語系為 `zh-TW`、`zh-HK`、`zh-MO` 或 `zh-Hant` 會自動選擇繁體中文；
明確指定的 `Hans` 或 `Hant` 文字系統優先於地區，因此 `zh-Hant-CN` 仍使用繁體中文，
`zh-Hans-TW` 仍使用簡體中文。原有的 `zh`、`zh-CN`、`zh-SG` 與 `zh-Hans` 設定維持簡體中文。

翻譯檔位於 `crates/ui-egui/src/i18n/zh-TW.tsv`，沿用現有的英文查表機制，涵蓋 368 個介面
項目，包含按鈕、選單、狀態、錯誤訊息、月份、表格欄位與各應用程式標語。譯文由英文原始字串
重新撰寫，採用「應用程式、資料夾、設定、解除安裝、下載、更新、系統匣、捷徑、儲存庫、檢查碼」
等台灣用語，並經台灣用語審核；沒有複製其他語系的翻譯。未翻譯的文字仍顯示英文。產品名稱
（ArtCraft Launcher、Launcher、Craft App Suite、PhotoCraft 等）與指令 ID 維持原文。

想在桌面版臨時試用而不改設定，可以設定環境變數：

```powershell
$env:ARTCRAFT_LAUNCHER_LOCALE = "zh-TW"
```

網頁版可在網址加上 `?lang=zh-TW`。

### Noto CJK 繁中字型

桌面版繁中介面優先使用已安裝的 **Noto Sans CJK TC**，其次使用支援的繁中系統字型：
macOS 的 PingFang TC／Heiti TC、Windows 的 Microsoft JhengHei／MingLiU，以及 Linux 與
FreeBSD 的 Noto Sans CJK TC。請從 [Noto CJK 官方專案](https://github.com/notofonts/noto-cjk)
下載 `Sans/OTF/TraditionalChinese/NotoSansCJKtc-Regular.otf`，並保留其 SIL Open Font License。
字型檔不放入 ArtCraft Launcher 原始碼庫，也不會在執行時自動下載。

- Windows：將檔案安裝到 `%LOCALAPPDATA%\Microsoft\Windows\Fonts` 或 `%WINDIR%\Fonts`，
  保留檔名 `NotoSansCJKtc-Regular.otf`。也可放在使用者字型目錄下的 `NotoSansCJKtc`
  子目錄，讓更新版本優先於系統中正在使用的舊字型。
- macOS：放在 `~/Library/Fonts` 或 `/Library/Fonts`。
- Linux：放在 `~/.local/share/fonts`、`~/.fonts` 或既有的 Noto 字型目錄。
  `NotoSansCJK-Regular.ttc` 會依字型家族名稱選用其中的台灣字形。

安裝後重新啟動 ArtCraft Launcher。找不到繁中字型時，介面會維持英文，並在設定中說明原因；
網頁版沒有系統字型可用，也維持英文。這項修改不新增瀏覽器字型下載或打包。

## English

Choose **繁體中文** in **Settings → Language**, or set the interface language to `zh-TW`. The
preference is saved and kept between launches. On first launch, a `zh-TW`, `zh-HK`, `zh-MO` or
`zh-Hant` system locale selects Traditional Chinese. An explicit `Hans` or `Hant` script takes
precedence over the region, so `zh-Hant-CN` still selects Traditional Chinese and `zh-Hans-TW`
still selects Simplified Chinese. Existing `zh`, `zh-CN`, `zh-SG` and `zh-Hans` preferences remain
Simplified Chinese.

The catalog is `crates/ui-egui/src/i18n/zh-TW.tsv`. It uses the existing English-keyed lookup
mechanism and covers 368 interface entries: buttons, menus, status and error messages, months,
table columns and app taglines. Translations were written from the English source strings with
Taiwan software terminology (應用程式, 資料夾, 設定, 解除安裝, 下載, 更新, 系統匣, 捷徑, 儲存庫,
檢查碼) and reviewed for Taiwan usage; no other catalog's translations were copied. Untranslated
strings fall back to English. Product names (ArtCraft Launcher, Launcher, Craft App Suite,
PhotoCraft and the other apps) and command IDs stay as published.

To try it on the desktop without changing the setting:

```powershell
$env:ARTCRAFT_LAUNCHER_LOCALE = "zh-TW"
```

For the web build, add `?lang=zh-TW` to the address.

### Noto CJK fonts

The desktop Traditional Chinese interface prefers an installed **Noto Sans CJK TC**, then
supported Traditional Chinese system faces: PingFang TC or Heiti TC on macOS, Microsoft JhengHei
or MingLiU on Windows, and Noto Sans CJK TC on Linux and FreeBSD. Download
`Sans/OTF/TraditionalChinese/NotoSansCJKtc-Regular.otf` from the
[official Noto CJK repository](https://github.com/notofonts/noto-cjk) and retain its SIL Open Font
License. Font binaries stay outside the ArtCraft Launcher repository and are never downloaded at
runtime.

- Windows: install the file in `%LOCALAPPDATA%\Microsoft\Windows\Fonts` or `%WINDIR%\Fonts`,
  keeping the filename `NotoSansCJKtc-Regular.otf`. The `NotoSansCJKtc` subdirectory of the
  per-user font directory is checked first, so an updated file can take precedence over a face
  already in use.
- macOS: use `~/Library/Fonts` or `/Library/Fonts`.
- Linux: use `~/.local/share/fonts`, `~/.fonts` or an existing Noto font directory. The merged
  `NotoSansCJK-Regular.ttc` is matched by its Traditional Chinese family name.

Restart ArtCraft Launcher after installing. Without a Traditional Chinese face the interface stays
in English and Settings explains why; the web build has no system font and does so too. This
change adds no browser font download or packaging.
