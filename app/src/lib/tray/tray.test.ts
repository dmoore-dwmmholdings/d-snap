// Tray popover (DSNA-128) against the mock Api.
import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import { createMockApi } from '../api';
import { windowKind } from '../desktop';
import TrayView from './TrayView.svelte';

describe('windowKind', () => {
  it('picks the tray view from the query string', () => {
    expect(windowKind('?view=tray')).toBe('tray');
    expect(windowKind('')).toBe('main');
    expect(windowKind('?view=other')).toBe('main');
  });
});

describe('TrayView', () => {
  it('shows the turn waiting for review and every folder', async () => {
    render(TrayView, { props: { api: createMockApi() } });
    await screen.findByText(/web-shop · Turn 5 finished/);
    expect(screen.getByText(/Add a payment step/)).toBeInTheDocument();
    expect(screen.getByText('Folder missing. Its snapshots are safe.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Locate' })).toBeInTheDocument();
    expect(screen.getByText(/Watching 2 folders/)).toBeInTheDocument();
  });

  it('undoes the turn after a confirmation and offers to put it back', async () => {
    const api = createMockApi();
    render(TrayView, { props: { api } });
    await screen.findByText(/Turn 5 finished/);
    await fireEvent.click(screen.getByRole('button', { name: 'Undo turn' }));
    expect(screen.getByText(/Files you changed since stay/)).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: 'Undo turn' }));
    await screen.findByText(/Turn 5 undone\. 1 file you changed since stayed\./);
    expect(screen.getByRole('button', { name: 'Put it back' })).toBeInTheDocument();
  });

  it('snapshots every folder with changes', async () => {
    const api = createMockApi();
    render(TrayView, { props: { api } });
    await screen.findByText(/Turn 5 finished/);
    await fireEvent.click(screen.getByRole('button', { name: 'Snapshot all now' }));
    await screen.findByText('Saved 1 folder.');
  });
});
