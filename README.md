# Wander(er) 📸

Wander(er) is a desktop media manager app for people who want:

- Local library control
- Telegram cloud backup
- Optional end-to-end style encryption before upload
- Easy cleanup with Cloud-Only mode
- Fast browsing, search, albums, and AI helpers

> [!NOTE]
> This is an initial Windows release, so expect some bugs or placeholder features.
> Android, macOS, and maybe Linux are planned later. Current focus is finishing the Windows roadmap first.

---

## ⚡ Quick Start (60 Seconds)

1. Install and open Wander(er)
2. Choose `Encrypted (Recommended)` on onboarding
3. Enter Telegram `API ID` + `API hash`
4. Sign in with Telegram verification code
5. Click `Import Files` and select photos/videos
6. Open `Uploads` and wait until done

Done. Your Telegram backup is running. ✅

---

## 📥 Get the App

If you are not technical, use a prebuilt release.

- Releases page (all versions): [Wander(er) Releases](https://github.com/ronimuliawan/Wanderer/releases)
- Direct download (Windows x64, v0.0.0): [Wanderer._0.0.0_x64-setup.exe](https://github.com/ronimuliawan/Wanderer/releases/download/0.0.0/Wanderer._0.0.0_x64-setup.exe)
- Direct download (Windows x64, latest): [Wanderer._0.0.0_x64-setup.exe (latest link)](https://github.com/ronimuliawan/Wanderer/releases/latest/download/Wanderer._0.0.0_x64-setup.exe)

1. Download the installer
2. Install and open Wander(er)

If you are running from source, see `For Developers (Optional)` at the bottom.

---

## 🚀 5-Minute Quick Start

1. Open the app
2. Complete onboarding (Encrypted is recommended)
3. Add your Telegram `API ID` and `API hash`
4. Connect your Telegram account with the code
5. Click `Import Files` and pick your photos/videos
6. Watch `Uploads` until queue is done
7. (Optional) Right-click uploaded items and use `Remove Local Copy` to save disk space

---

## ✨ What This App Does

- Imports photos and videos into your local library
- Uploads them to your Telegram account (Saved Messages)
- Lets you browse by timeline, albums, favorites, map, people, tags, and duplicates
- Lets you remove local files while keeping cloud copies (`Cloud-Only`)
- Lets you restore local copies later from Telegram
- Supports export, trash/restore, and database backup

---

## 🧾 Before You Start

You need:

- Windows 10 or 11
- A Telegram account
- Internet connection for upload/download
- Telegram API credentials (`API ID` and `API hash`) for BYOK onboarding

To get Telegram API credentials:

1. Go to `https://my.telegram.org`
2. Sign in with your Telegram number
3. Open `API development tools`
4. Create an app entry (once)
5. Copy `api_id` and `api_hash`

---

## 🛡️ First Launch Setup (Onboarding)

When you open Wander(er) for the first time, it shows a setup flow.

### 1. Choose Protection Mode

- `Encrypted (Recommended)`:
  - Files are encrypted before Telegram cloud upload
  - Better privacy for Telegram cloud backups
- `Unencrypted`:
  - Faster setup
  - Cloud copies are plaintext

Important:

- If you enable encryption, it is one-way in current UI.
- You cannot switch back to unencrypted mode without a full reset.

### 2. If Encrypted: Set Passphrase + Save Recovery Key

- Create a passphrase (minimum 8 chars)
- Save your one-time recovery key (download/print/copy)
- Verify the required key segments to continue

Important:

- If you lose both passphrase and recovery key, encrypted data is unrecoverable.

### 3. Enter BYOK Telegram Credentials

- Paste your `API ID` and `API hash`
- These are stored locally with Windows DPAPI

### 4. Connect Telegram

- Enter phone number
- Request verification code
- Enter code and finish onboarding

---

## 🗂️ Daily Use

### Import

- Click `Import Files` in the sidebar
- Select one or more files
- Files appear in timeline

### Upload

- Open `Uploads` to monitor queue progress
- Failed uploads can be retried

### Organize

- Use `Albums`, `Favorites`, `Archive`, `Trash`
- Use ratings and bulk actions for faster cleanup

### Free Disk Space (Cloud-Only)

1. Right-click an uploaded item
2. Choose `Remove Local Copy`
3. Item stays in cloud and shows cloud-only state

To restore later:

1. Right-click cloud-only item
2. Choose `Download Local Copy`

### View Cloud-Only Items

- Click to open
- App downloads on demand via `view_cache`

### Export

- Right-click item and choose export
- Bulk export is available via selection mode

### Share Link (Important)

- `Copy Share Link` creates a Telegram deep link (`tg://...`)
- It is mainly useful for your own Telegram app/account context
- It is not a public web sharing link

### Backup Database

- Go to `Settings -> Storage`
- Use database backup actions
- In encrypted mode, backup file is exported as encrypted `.db.wbenc`

---

## 🔐 Security & Privacy (Current Behavior)

In `Encrypted` mode:

- Cloud uploads are encrypted before sending
- Thumbnails are encrypted at rest
- View cache is encrypted at rest
- Database backup artifact is encrypted

Important current limitation:

- Local files in `backup/` are still plaintext at rest
- This means anyone with local filesystem access can copy originals from `backup/`

Planned next improvement:

- Full local at-rest encryption for `backup/` folder

---

## 🤖 AI Features

AI is opt-in.

- Default: `OFF`
- Enable in `Settings -> AI`
- Models download when needed

Current AI-related features include:

- People (face grouping)
- Tags
- Duplicate detection
- Semantic search (model/device compatibility may vary)

---

## 🎨 Display and Personalization

Go to `Settings -> Display` to customize:

- Theme preset (including iOS 26 and Android 16 inspired themes)
- Appearance mode (for supported themes)
- Corner style
- Icon style
- Timeline grouping (day/month/year)

---

## 💾 Storage Locations

Main app data folder:

- `%LOCALAPPDATA%\com.wanderer.desktop\`

Important files/folders inside:

- `library.db` (database)
- `backup\` (local media library)
- `view_cache\` (on-demand cloud view cache)
- `cache\thumbnails\` (thumbnail cache)
- `models\` (AI models)
- `session.db` (Telegram session)

## ⚠️ Known Limitations

- RAW support is partial:
  - Many RAW types are accepted
  - Some files may still fail thumbnail/viewer rendering
- Mobile companion app is not implemented
- Telegram metadata preservation is incomplete for some sync/restore scenarios

---

## 🛠️ Troubleshooting

### Sign-in does not continue after entering code

- Go back and request a new code
- Re-check API ID/API hash
- Restart app and try again

### AI tags/people not appearing

- Confirm AI toggles are ON in `Settings -> AI`
- Confirm models are downloaded
- Process pending images

### Cloud restore fails with message not found

- The referenced Telegram message may be deleted already
- If a cloud message was removed externally, that specific local restore cannot be completed

### Cache using too much space

- Use `Settings -> Storage`
- Lower cache size/retention sliders

### Need a clean reset

Delete:

- `%LOCALAPPDATA%\com.wanderer.desktop\library.db`
- `%LOCALAPPDATA%\com.wanderer.desktop\session.db` (optional, forces Telegram re-login)

---

## ℹ️ About and Links

In app:

- `Settings -> About`

This section shows:

- App version
- Project link(s)
- Channel/group/support links (if configured)

---

## 👩‍💻 For Developers (Optional)

If you are building from source:

```bash
npm install
npm run tauri dev
```

The MCP agent bridge (`tauri-plugin-mcp-bridge`) is **not** compiled in by default. It
starts an unauthenticated WebSocket server that can run arbitrary script inside the app,
so it is an opt-in, debug-only feature and is bound to localhost when enabled:

```bash
npm run tauri dev -- --features mcp-bridge
```

Production build:

```bash
npm run build
```
