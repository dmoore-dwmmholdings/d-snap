// Chain H (DSNA-41/42/43): the shell end to end on the mock Api.
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import App from '../../App.svelte';
import { createMockApi, type MockApi } from '../api';

function setup(api: MockApi = createMockApi({ now: () => Date.UTC(2026, 9, 1, 12) })) {
  const r = render(App, { props: { api } });
  return { api, ...r };
}

async function openProject(name: string) {
  const row = await screen.findByRole('option', { name: new RegExp(`^${name}`) });
  await fireEvent.click(row);
  await screen.findByRole('heading', { level: 2, name });
}

describe('sidebar', () => {
  it('lists projects by name with change badges and a missing marker', async () => {
    setup();
    const list = await screen.findByRole('listbox', { name: 'Projects' });
    await waitFor(() => expect(within(list).getByLabelText(/\d+ changed/)).toBeInTheDocument());
    const names = within(list)
      .getAllByRole('option')
      .map((o) => o.textContent?.trim().split(/\s/)[0]);
    expect(names).toEqual(['notes', 'rust-tool', 'web-shop']);
    expect(within(list).getByLabelText('folder missing')).toBeInTheDocument();
    // rust-tool has no unsaved changes: no badge.
    const rust = within(list).getByRole('option', { name: /^rust-tool/ });
    expect(within(rust).queryByLabelText(/changed/)).toBeNull();
  });

  it('adds a project from the folder picker', async () => {
    const { api } = setup();
    const add = vi.spyOn(api, 'addProject');
    api.mockSetPickFolderResult('C:\\Work\\new-thing');
    await fireEvent.click(await screen.findByRole('button', { name: '+ Add project' }));
    await waitFor(() => expect(add).toHaveBeenCalledWith('C:\\Work\\new-thing'));
    await screen.findByRole('heading', { level: 2, name: 'new-thing' });
  });

  it('renames inline and removes with the delete-snapshots option', async () => {
    const { api } = setup();
    const remove = vi.spyOn(api, 'removeProject');
    const row = await screen.findByRole('option', { name: /^rust-tool/ });
    await fireEvent.contextMenu(row);
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Rename' }));
    const input = screen.getByLabelText('Project name');
    await fireEvent.input(input, { target: { value: 'cli-tool' } });
    await fireEvent.keyDown(input, { key: 'Enter' });
    await screen.findByRole('option', { name: /^cli-tool/ });

    await fireEvent.contextMenu(screen.getByRole('option', { name: /^cli-tool/ }));
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Remove…' }));
    const dialog = screen.getByRole('dialog');
    await fireEvent.click(within(dialog).getByRole('checkbox'));
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Remove' }));
    await waitFor(() => expect(remove).toHaveBeenCalledWith(expect.any(Number), true));
    await waitFor(() => expect(screen.queryByRole('option', { name: /^cli-tool/ })).toBeNull());
  });
});

describe('project header', () => {
  it('enables Snapshot only with changes, and Enter in the label snapshots', async () => {
    const { api } = setup();
    await openProject('rust-tool');
    const button = screen.getByRole('button', { name: 'Snapshot' });
    await waitFor(() => expect(button).toBeDisabled());

    const p = (await api.listProjects()).find((x) => x.name === 'rust-tool')!;
    api.mockWriteFile(p.id, 'src/new.rs', 'fn x() {}\n');
    await waitFor(() => expect(button).toBeEnabled());

    const snap = vi.spyOn(api, 'snapshot');
    const label = screen.getByLabelText('Snapshot label');
    await fireEvent.input(label, { target: { value: 'my label' } });
    await fireEvent.keyDown(label, { key: 'Enter' });
    await waitFor(() => expect(snap).toHaveBeenCalledWith(p.id, 'my label'));
    await waitFor(() => expect(button).toBeDisabled());
  });

  it('shows what the snapshot left out', async () => {
    setup();
    await openProject('web-shop');
    await fireEvent.click(screen.getByRole('button', { name: 'Snapshot' }));
    const warn = await screen.findByRole('status', { name: 'Snapshot warnings' });
    expect(within(warn).getByText(/left out of the last snapshot/)).toBeInTheDocument();
    expect(within(warn).getByText('videos/demo.mp4')).toBeInTheDocument();
  });
});

describe('versions list', () => {
  it('shows Unsaved changes first, then versions newest first', async () => {
    setup();
    await openProject('web-shop');
    const list = screen.getByRole('listbox', { name: 'Versions' });
    const rows = await within(list).findAllByRole('option');
    expect(rows[0]).toHaveTextContent('Unsaved changes');
    await waitFor(() => expect(rows[0]).toHaveTextContent(/\d+ files changed/));
    expect(rows[1]).toHaveTextContent('after agent turn');
    expect(rows.at(-1)).toHaveTextContent('Initial import');
  });

  it('reads "No changes" when the folder matches the latest version', async () => {
    setup();
    await openProject('rust-tool');
    const unsaved = await screen.findByRole('option', { name: /Unsaved changes/ });
    await waitFor(() => expect(unsaved).toHaveTextContent('No changes'));
  });

  it('restores the project: plan, confirm, restore, then selects the safety version', async () => {
    const { api } = setup();
    await openProject('web-shop');
    const calls: string[] = [];
    vi.spyOn(api, 'restorePlan').mockImplementation(async (...a) => {
      calls.push('plan');
      return api.constructor.prototype.restorePlan.apply(api, a);
    });
    vi.spyOn(api, 'restoreProject').mockImplementation(async (...a) => {
      calls.push('restore');
      return api.constructor.prototype.restoreProject.apply(api, a);
    });
    await fireEvent.click(
      await screen.findByRole('button', { name: 'Restore project to Working checkout' }),
    );
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText(/safety snapshot is taken first/)).toBeInTheDocument();
    expect(within(dialog).getByText(/Left alone/)).toBeInTheDocument();
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Restore' }));
    await waitFor(() => expect(calls).toEqual(['plan', 'restore']));
    const versions = screen.getByRole('listbox', { name: 'Versions' });
    await waitFor(() =>
      expect(within(versions).getByRole('option', { selected: true })).toHaveTextContent(
        'Before restore to Working checkout',
      ),
    );
  });
});

describe('changes view', () => {
  it('lists unsaved changes with status letters and filters them', async () => {
    setup();
    await openProject('web-shop');
    const files = await screen.findByRole('listbox', { name: 'Changed files' });
    const rows = within(files).getAllByRole('option');
    const text = rows.map((r) => r.textContent ?? '');
    expect(text.some((t) => t.startsWith('M') && t.includes('cart.ts'))).toBe(true);
    expect(text.some((t) => t.startsWith('A') && t.includes('config.ts'))).toBe(true);
    expect(text.some((t) => t.startsWith('D') && t.includes('payment.ts'))).toBe(true);

    await fireEvent.input(screen.getByLabelText('Filter files'), { target: { value: 'CART' } });
    expect(within(files).getAllByRole('option')).toHaveLength(1);
    expect(screen.getByText(new RegExp(`^1 of ${rows.length} files`))).toBeInTheDocument();
  });

  it('restores one file after confirming', async () => {
    const { api } = setup();
    await openProject('web-shop');
    const restore = vi.spyOn(api, 'restoreFile');
    await fireEvent.click(await screen.findByRole('button', { name: 'Restore src/payment.ts' }));
    const dialog = await screen.findByRole('dialog');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Restore file' }));
    await waitFor(() =>
      expect(restore).toHaveBeenCalledWith(
        expect.any(Number),
        expect.any(Number),
        'src/payment.ts',
      ),
    );
    // The newest version is now the safety snapshot, which lacks the file: it shows as added.
    const files = screen.getByRole('listbox', { name: 'Changed files' });
    await waitFor(() =>
      expect(within(files).getByRole('option', { name: /payment\.ts/ })).toHaveTextContent(/^A/),
    );
    expect(screen.getByRole('option', { name: /Unsaved changes/ })).toBeInTheDocument();
    await screen.findByText(/Restored src\/payment\.ts/);
  });

  it('opens the diff of a selected file', async () => {
    setup();
    await openProject('web-shop');
    const files = await screen.findByRole('listbox', { name: 'Changed files' });
    await fireEvent.click(within(files).getByRole('option', { name: /cart\.ts/ }));
    await screen.findByRole('button', { name: 'Restore this file' });
    expect(await screen.findAllByText(/^@@ -\d+,\d+ \+\d+,\d+ @@$/)).not.toHaveLength(0);
  });

  it('shows the missing-folder state', async () => {
    setup();
    await openProject('notes');
    expect(await screen.findByText(/Folder not found/)).toBeInTheDocument();
    expect(screen.getByText(/project folder is missing/)).toBeInTheDocument();
  });
});

describe('keyboard', () => {
  it('moves through versions with the arrow keys', async () => {
    setup();
    await openProject('web-shop');
    const list = screen.getByRole('listbox', { name: 'Versions' });
    await within(list).findAllByRole('option');
    await fireEvent.keyDown(list, { key: 'ArrowDown' });
    await waitFor(() =>
      expect(within(list).getByRole('option', { selected: true })).not.toHaveTextContent(
        'Unsaved changes',
      ),
    );
    await fireEvent.keyDown(list, { key: 'Home' });
    await waitFor(() =>
      expect(within(list).getByRole('option', { selected: true })).toHaveTextContent(
        'Unsaved changes',
      ),
    );
  });
});
