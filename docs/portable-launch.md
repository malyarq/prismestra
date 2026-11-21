# Portable launch

Extract the complete Windows ZIP into a folder you can write to. Prismestra locates portable data beside the executable, independent of the current working directory. Portable startup uses your existing Windows account. The native session can start an explicitly configured [managed OpenRGB backend](managed-openrgb.md).

## First setup

1. Open `Prismestra.exe` from the extracted folder. With the adjacent release manifest, this opens a software preview without activating saved hardware outputs.
2. Create your device layout or open an exported profile using the editor. Inspect the Connections page and configure the supported devices on this PC. Saving connection settings and applying them are separate actions. See [connection behavior](https://github.com/malyarq/prismestra/blob/codex/native-design/docs/universal-connections.md).
3. Save your profile as `data/profile.json` inside the extracted folder. The launcher does not guess which of your scenes to use and does not automatically save unsaved profile edits. Use **Export Profile** to move a profile with its LCD media to another PC. Opening the ZIP imports those files into the local asset store. Save the imported profile at `data/profile.json` afterward.
4. To activate the saved connections, create a Windows shortcut. Set its target to `"C:\path\to\Prismestra.exe" --portable --start-connected`. Exit the existing session from the tray before switching modes. Start the new shortcut to restore the saved scene and its local connections, including configured temperature routes.

Preview opens `data/profile.json` when it exists and the default scene otherwise. Connected mode requires both the profile and `data/state/lighting-local.json`. Missing inputs show a startup error. The application validates the profile, saved connections and device capabilities at runtime.

Closing the editor leaves the resident engine in the tray. Exit from its menu to stop the session. Repeating the direct launch for the same portable folder opens the editor in the existing session. To switch modes, save changes and exit before starting the other shortcut.

If Windows temporarily refuses the tray icon, the engine continues and registration retries with backoff. Until the icon is registered, closing the editor keeps the window accessible instead of hiding the only entry point. Explorer/taskbar recreation triggers a new registration attempt; restarting Explorer or another application is not an automatic recovery action.

Native startup errors appear in a dialog. After dismissing the dialog, the process exits with a nonzero status. Device readiness is reported separately in the application.

## What moves with the folder

| Location | Contents |
| --- | --- |
| `data/profile.json` | Saved scene, layout, capture and LCD configuration |
| `data/state/` | Local connection bindings, sensor routes, imported media, scene library and editor state |
| Windows Credential Manager | Home Assistant tokens, outside the folder |

The release ZIP contains no personal profile or state. Your populated folder contains private device details; do not upload or redistribute it. **Export Profile** includes the current profile and its referenced media. The named **My Scenes** library stays in `data/state/scene-library.json` when using the portable launchers. Back up that file separately if you want to keep the library. Local bindings and credentials stay on this PC. On another PC, inspect or replace local connections and temperature sources before activating outputs. Home Assistant credentials must be entered on that PC. Device identity checks are retained; copying files does not grant compatibility with a replacement controller.

## OpenRGB and prerequisites

For devices using the OpenRGB adapter, configure [managed local OpenRGB](managed-openrgb.md) or run a compatible external OpenRGB SDK6 server and set its endpoint in Connections. Direct BLE, Skydimo, LCD, WLED and Home Assistant paths do not become OpenRGB devices through this launcher. Another application's ownership can prevent activation. Keep your fan and pump software running; Prismestra does not control cooling.

The package retains the Windows x64, Visual C++ v14 and WebView2 requirements described in `START-HERE.md`. To use the classic editor, add `--portable --classic-editor` to the Windows shortcut target. Add `--start-connected` there when you want to activate the saved connections.

Keep the application and its data together when updating. Exit the session first, back up your data, and replace program files while preserving `data`.

## Compatibility with the previous launchers

From the extracted folder:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File ".\Launch Prismestra.ps1" -Check
powershell.exe -NoProfile -ExecutionPolicy Bypass -File ".\Launch Prismestra.ps1" -Connected -Check
```

The previous launchers remain available while existing tasks are verified. Their `-Check` option reports planned executable and arguments without starting Prismestra or contacting devices. The previous launcher waits up to 15 seconds for native-core startup acknowledgement. Direct portable launch uses the GUI process itself as the entry point.

[Native entry point](../src/ui/windows/portable.rs) · [Startup and validation](../src/ui/windows.rs) · [Previous launcher](../scripts/portable/Launch%20Prismestra.ps1) · [Windows CI](../.github/workflows/windows.yml)
