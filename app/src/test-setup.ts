import '@testing-library/jest-dom/vitest';

// jsdom has no layout: element size bindings need a ResizeObserver that never fires.
class NoopResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
globalThis.ResizeObserver ??= NoopResizeObserver as unknown as typeof ResizeObserver;
