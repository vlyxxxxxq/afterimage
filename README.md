# Afterimage

Afterimage is a Rust desktop app that keeps the last two minutes of gameplay ready in the Windows tray. Press **Ctrl + Shift + F8** to save the last 30 seconds. Pick 15, 30, 45, 60, 90, or 120 seconds in Settings.

[Project page](https://vlyxxxxxq.github.io/afterimage/) · [Windows download](https://github.com/vlyxxxxxq/afterimage/releases/latest)

## Install on Windows x64

1. Install FFmpeg with `ffmpeg.exe` and `ffprobe.exe` on your PATH. The build needs gdigrab, libx264, and AAC. Windows Package Manager users can run `winget install --id Gyan.FFmpeg -e`.
2. Install the Microsoft Edge WebView2 Runtime if your Windows installation lacks it.
3. Download and extract the Windows ZIP from Releases. Double-click **Install.cmd**. It installs for your account, adds a Start menu entry, and starts Afterimage in the tray at sign-in. The executable is unsigned.
4. Double-click the tray icon to open the library. Right-click it to save, pause, or exit. Opening the executable directly also runs it portably. Use `afterimage.exe --show` to open the window immediately.

Capture begins as soon as the app starts. An early save keeps the history available so far. Replays end on the last completed one-second segment. Capture uses the selected display, the default Windows playback device, and a CPU H.264 encoder. Settings include monitor, 30/60 fps, and a 720p/1080p/1440p resolution limit. Protected content and exclusive fullscreen capture depend on the game and driver.

## Edit a clip

Search by title or tag, filter favorites, or sort by date or title. In the editor, enter comma-separated tags next to the title and select **Save details**. The replay shortcut also works during an import or export. Import an existing video, or select a replay to open the editor. Drag the orchid handles to trim, or scrub and press **I** and **O**. **Space** previews the selection. The arrow keys move by a tenth of a second. Hold **Shift** for 10 ms nudges, or type an exact in/out time in seconds. Loop the selection at 0.25× through 2× to review timing. **Ctrl+Z** and **Ctrl+Y** undo and redo trim, crop, and mute changes during the current edit. Export quality offers High quality, Balanced, and Smaller file presets. Preview speed only affects playback in the editor. Choose original, vertical, or square framing, and optionally mute audio before exporting a new MP4.

## Storage and upgrades

Saved clips use the Windows Videos folder under `Afterimage`. Settings, titles, favorites, and edit selections use `%LOCALAPPDATA%\Afterimage`. The Rust version reads the earlier Windows app's settings, library, and edit files. Installing while that app is active stages the Rust version for the next sign-in. Save any recent gameplay you want to keep before exiting an active recorder.

To disable sign-in launch, use Windows Startup Apps. To uninstall, exit via the tray, delete the Afterimage shortcut from your Startup folder and Start menu, then remove the installed Rust version under `%LOCALAPPDATA%\Programs\Afterimage`. Clips and preferences live separately.

## Build

Use a Windows x64 Rust MSVC toolchain, Visual Studio C++ Build Tools, and a Windows SDK.

```powershell
cargo build --release --locked
cargo test --release --locked
```

The media integration test uses FFmpeg and FFprobe on PATH. The application binary is `target\release\afterimage.exe`. Its embedded interface uses WebView2. Rust owns recording, WASAPI loopback capture, replay assembly, library storage, hotkey and tray handling, and export orchestration. The `docs/` directory is the static GitHub Pages site.
