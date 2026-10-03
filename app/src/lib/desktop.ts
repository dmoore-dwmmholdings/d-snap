// Window and tray glue for the desktop app (DSNA-128). Every function is a no-op outside
// Tauri (browser dev server, tests), so the views can call them freely.
import { inTauri, type ProjectId, type TurnId, type Unsubscribe } from './api';

/** Tray icon id (`tray::TRAY_ID` in the Rust app). */
const TRAY_ID = 'dsnap-tray';
/** Main window asks to open a turn (sent by the tray popover). */
const OPEN_TURN = 'dsnap://open-turn';

/** What the main window should show when the tray opens it. */
export interface OpenTarget {
  projectId: ProjectId;
  turnId: TurnId | null;
}

/** Which view this window renders: the tray popover loads `index.html?view=tray`. */
export function windowKind(search: string = globalThis.location?.search ?? ''): 'main' | 'tray' {
  return new URLSearchParams(search).get('view') === 'tray' ? 'tray' : 'main';
}

/** Show and focus the main window, optionally on a turn; then hide this popover. */
export async function openMain(target?: OpenTarget): Promise<void> {
  if (!inTauri()) return;
  const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
  const main = await WebviewWindow.getByLabel('main');
  if (main) {
    await main.unminimize();
    await main.show();
    await main.setFocus();
  }
  if (target) {
    const { emitTo } = await import('@tauri-apps/api/event');
    await emitTo('main', OPEN_TURN, target);
  }
  await hideSelf();
}

/** Hide the current window (the popover). */
export async function hideSelf(): Promise<void> {
  if (!inTauri()) return;
  const { getCurrentWindow } = await import('@tauri-apps/api/window');
  if (getCurrentWindow().label === 'tray') await getCurrentWindow().hide();
}

/** Called in the main window when the tray asks it to open a turn. */
export function onOpenTarget(cb: (t: OpenTarget) => void): Unsubscribe {
  if (!inTauri()) return () => {};
  let stop: (() => void) | null = null;
  let stopped = false;
  void import('@tauri-apps/api/event').then(({ listen }) =>
    listen<OpenTarget>(OPEN_TURN, (e) => cb(e.payload)).then((un) => {
      if (stopped) un();
      else stop = un;
    }),
  );
  return () => {
    stopped = true;
    stop?.();
  };
}

/** Show how many turns wait for review next to the tray icon (title on macOS) and in its
 * tooltip. */
export async function setTrayWaiting(waiting: number): Promise<void> {
  if (!inTauri()) return;
  try {
    const { TrayIcon } = await import('@tauri-apps/api/tray');
    const tray = await TrayIcon.getById(TRAY_ID);
    if (!tray) return;
    await tray.setTitle(waiting > 0 ? String(waiting) : null);
    await tray.setTooltip(
      waiting > 0
        ? `d-snap · ${waiting} ${waiting === 1 ? 'turn waits' : 'turns wait'} for review`
        : 'd-snap',
    );
  } catch {
    // No tray on this desktop.
  }
}
