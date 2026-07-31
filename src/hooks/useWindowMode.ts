import { useEffect } from "react";
import { currentMonitor, getCurrentWindow, LogicalPosition, LogicalSize } from "@tauri-apps/api/window";
import type { AppScreen } from "../app/appFlow";

const TASKBAR_SIZE = new LogicalSize(326, 88);
const TASKBAR_MIN_SIZE = new LogicalSize(306, 80);
const APP_SIZE = new LogicalSize(820, 760);
const APP_MIN_SIZE = new LogicalSize(520, 560);
const BOTTOM_MARGIN = 28;

const TASKBAR_SCREENS: AppScreen[] = ["welcome", "ready", "session"];

async function placeBottomCenter(width: number, height: number) {
  const monitor = await currentMonitor();
  if (!monitor) return;

  const workPosition = monitor.workArea.position.toLogical(monitor.scaleFactor);
  const workSize = monitor.workArea.size.toLogical(monitor.scaleFactor);
  const x = workPosition.x + (workSize.width - width) / 2;
  const y = workPosition.y + workSize.height - height - BOTTOM_MARGIN;

  await getCurrentWindow().setPosition(new LogicalPosition(Math.round(x), Math.round(y)));
}

async function placeCenter(width: number, height: number) {
  const monitor = await currentMonitor();
  if (!monitor) return;

  const workPosition = monitor.workArea.position.toLogical(monitor.scaleFactor);
  const workSize = monitor.workArea.size.toLogical(monitor.scaleFactor);
  const x = workPosition.x + (workSize.width - width) / 2;
  const y = workPosition.y + (workSize.height - height) / 2;

  await getCurrentWindow().setPosition(new LogicalPosition(Math.round(x), Math.round(y)));
}

export function useWindowMode(screen: AppScreen, enabled = true) {
  useEffect(() => {
    if (!enabled) return;

    const win = getCurrentWindow();
    const compact = TASKBAR_SCREENS.includes(screen);
    const size = compact ? TASKBAR_SIZE : APP_SIZE;

    win.setMinSize(compact ? TASKBAR_MIN_SIZE : APP_MIN_SIZE).catch(() => {});
    win.setSize(size)
      .then(() => (compact ? placeBottomCenter(size.width, size.height) : placeCenter(size.width, size.height)))
      .catch(() => {});
  }, [enabled, screen]);
}
