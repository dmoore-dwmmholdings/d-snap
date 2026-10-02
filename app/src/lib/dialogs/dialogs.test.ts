// Chain J (DSNA-48): shared dialogs and toasts.
import { fireEvent, render, screen, within } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import type { RestorePlan, RestoreReport, Version } from '../api';
import ToastHost from '../toast/ToastHost.svelte';
import { Toasts } from '../toast/toasts.svelte';
import ConfirmRestoreDialog from './ConfirmRestoreDialog.svelte';
import ProgressOverlay from './ProgressOverlay.svelte';
import RemoveProjectDialog from './RemoveProjectDialog.svelte';
import RestoreResultDialog from './RestoreResultDialog.svelte';
import SnapshotWarnings from './SnapshotWarnings.svelte';
import { ApiError } from '../api';

const version: Version = {
  id: 5,
  projectId: 1,
  label: 'Working checkout',
  createdAtMs: 0,
  kind: 'manual',
  pinned: false,
  unstable: false,
  counts: { added: 0, modified: 0, deleted: 0 },
};
const plan: RestorePlan = {
  target: 5,
  write: ['a.ts', 'b.ts'],
  delete: ['new.ts'],
  createDirs: ['logs'],
  uncaptured: ['big.bin'],
};

describe('ConfirmRestoreDialog', () => {
  it('lists every group with counts, warns about uncaptured paths, and is danger on deletes', async () => {
    const onconfirm = vi.fn();
    const oncancel = vi.fn();
    render(ConfirmRestoreDialog, { props: { version, plan, onconfirm, oncancel } });
    const d = screen.getByRole('dialog', { name: /Restore project to/ });
    expect(within(d).getByText('Overwrite 2 files (2)')).toBeInTheDocument();
    expect(within(d).getByText('Delete 1 file added since (1)')).toBeInTheDocument();
    expect(within(d).getByText('Recreate 1 empty folder (1)')).toBeInTheDocument();
    expect(within(d).getByText('big.bin')).toBeInTheDocument();
    expect(within(d).getByText(/safety snapshot is taken first/)).toBeInTheDocument();
    const restore = within(d).getByRole('button', { name: 'Restore' });
    expect(restore).toHaveClass('danger');
    expect(within(d).getByRole('button', { name: 'Cancel' })).toHaveFocus();
    await fireEvent.click(restore);
    expect(onconfirm).toHaveBeenCalled();
    await fireEvent.keyDown(d, { key: 'Escape' });
    expect(oncancel).toHaveBeenCalled();
  });

  it('confirms a single file', () => {
    render(ConfirmRestoreDialog, {
      props: { version, file: 'src/x.ts', onconfirm: () => {}, oncancel: () => {} },
    });
    expect(screen.getByRole('dialog', { name: 'Restore x.ts?' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Restore file' })).not.toHaveClass('danger');
  });
});

describe('RemoveProjectDialog', () => {
  it('passes the delete-snapshots choice', async () => {
    const onconfirm = vi.fn();
    render(RemoveProjectDialog, {
      props: {
        project: {
          id: 1,
          name: 'p',
          root: 'C:/p',
          missing: false,
          settings: { extraIgnore: [], respectGitignore: true, autoSnapshot: { kind: 'off' } },
        },
        onconfirm,
        oncancel: () => {},
      },
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Remove' }));
    expect(onconfirm).toHaveBeenLastCalledWith(false);
    await fireEvent.click(screen.getByRole('checkbox'));
    await fireEvent.click(screen.getByRole('button', { name: 'Remove' }));
    expect(onconfirm).toHaveBeenLastCalledWith(true);
  });
});

describe('SnapshotWarnings', () => {
  it('groups skipped files by reason and lists unstable ones', () => {
    render(SnapshotWarnings, {
      props: {
        skipped: [
          { path: 'big.iso', reason: { kind: 'tooLarge', size: 60 * 1024 * 1024 } },
          { path: 'db.lock', reason: { kind: 'locked' } },
          { path: 'cloud.doc', reason: { kind: 'unreadable', msg: 'online-only cloud file' } },
        ],
        unstable: ['app.log'],
      },
    });
    expect(screen.getByText('Larger than the size cap (1)')).toBeInTheDocument();
    expect(screen.getByText(/60 MB/)).toBeInTheDocument();
    expect(screen.getByText('Locked by another program (1)')).toBeInTheDocument();
    expect(screen.getByText(/online-only cloud file/)).toBeInTheDocument();
    expect(screen.getByText('Kept changing while captured (1)')).toBeInTheDocument();
  });
});

describe('RestoreResultDialog', () => {
  it('lists failures and offers undo', async () => {
    const report: RestoreReport = {
      safetyVersion: 9,
      written: ['a'],
      deleted: [],
      failed: [['b.txt', 'b.txt is locked by another program']],
      uncaptured: [],
    };
    const onundo = vi.fn();
    render(RestoreResultDialog, { props: { report, onundo, onclose: () => {} } });
    expect(screen.getByText(/b.txt is locked/)).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: 'Undo using safety snapshot' }));
    expect(onundo).toHaveBeenCalled();
  });
});

describe('ProgressOverlay', () => {
  it('shows the phase and cancels', async () => {
    const oncancel = vi.fn();
    render(ProgressOverlay, {
      props: {
        progress: {
          opId: 'op-1',
          projectId: 1,
          op: 'snapshot',
          phase: 'hash',
          done: 3,
          total: 10,
          path: null,
        },
        oncancel,
      },
    });
    expect(screen.getByText('Saving changed files… 3 of 10')).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(oncancel).toHaveBeenCalled();
  });
});

describe('toasts', () => {
  it('shows ApiError code and message, and runs actions', async () => {
    const toasts = new Toasts();
    render(ToastHost, { props: { toasts } });
    const run = vi.fn();
    toasts.error('Restore failed', new ApiError('io', 'disk full'), { label: 'Undo', run });
    expect(await screen.findByRole('alert')).toHaveTextContent('Restore failed: disk full (io)');
    await fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
    expect(run).toHaveBeenCalled();
    expect(screen.queryByRole('alert')).toBeNull();
  });
});
