// Transient notifications (J2). Errors stay until dismissed; others fade after a while.

import { ApiError } from '../api';

export type ToastKind = 'info' | 'success' | 'warning' | 'error';

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
  /** Optional button, e.g. "Undo". */
  action?: { label: string; run: () => void };
}

const TIMEOUT_MS: Record<ToastKind, number | null> = {
  info: 4000,
  success: 4000,
  warning: 8000,
  error: null,
};

export class Toasts {
  items = $state<Toast[]>([]);
  private next = 1;

  push(kind: ToastKind, message: string, action?: Toast['action']): number {
    const id = this.next++;
    this.items.push({ id, kind, message, action });
    const ms = TIMEOUT_MS[kind];
    if (ms !== null) setTimeout(() => this.dismiss(id), ms);
    return id;
  }

  /** An error toast: `context: message (code)`. */
  error(context: string, err: unknown, action?: Toast['action']): number {
    const e = ApiError.from(err);
    return this.push('error', `${context}: ${e.message} (${e.code})`, action);
  }

  dismiss(id: number): void {
    this.items = this.items.filter((t) => t.id !== id);
  }
}
