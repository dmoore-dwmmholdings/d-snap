// Chain P (DSNA-70/71/72): compare two versions, version management, hunk revert.
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import App from '../../App.svelte';
import { ApiError, createMockApi } from '../api';

async function setup() {
  const api = createMockApi({ now: () => Date.UTC(2026, 9, 1, 12) });
  render(App, { props: { api } });
  await fireEvent.click(await screen.findByRole('option', { name: /^web-shop/ }));
  const versions = await screen.findByRole('listbox', { name: 'Versions' });
  await within(versions).findAllByRole('option');
  const list = await api.listVersions(
    (await api.listProjects()).find((p) => p.name === 'web-shop')!.id,
  );
  const byLabel = (l: string) => list.find((v) => v.label === l)!;
  const row = (l: string) => {
    const r = within(versions)
      .getAllByRole('option')
      .find((o) => o.querySelector('.label')?.textContent?.replace(/[📌⚠]/gu, '').trim() === l);
    if (!r) throw new Error(`no version row "${l}"`);
    return r;
  };
  return { api, versions, byLabel, row };
}

describe('compare two versions (F16)', () => {
  it('picks A and B, shows A → B, swaps, restores explicitly, and exits', async () => {
    const { api, byLabel, row } = await setup();
    const changes = vi.spyOn(api, 'changes');
    await fireEvent.click(screen.getByRole('button', { name: 'Compare…' }));
    // Pick the newer one first: the order is fixed to older → newer.
    await fireEvent.click(row('New logo'));
    await fireEvent.click(row('Working checkout'));
    const a = byLabel('Working checkout').id;
    const b = byLabel('New logo').id;
    await waitFor(() =>
      expect(changes).toHaveBeenLastCalledWith(expect.any(Number), a, { kind: 'version', id: b }),
    );
    expect(
      screen.getByRole('heading', { name: 'A: Working checkout → B: New logo' }),
    ).toBeInTheDocument();
    const files = await screen.findByRole('listbox', { name: 'Changed files' });
    expect(within(files).getAllByRole('button', { name: / to A$/ }).length).toBeGreaterThan(0);
    expect(within(files).getAllByRole('button', { name: / to B$/ }).length).toBeGreaterThan(0);

    await fireEvent.click(screen.getByRole('button', { name: 'Swap A ⇄ B' }));
    await waitFor(() =>
      expect(changes).toHaveBeenLastCalledWith(expect.any(Number), b, { kind: 'version', id: a }),
    );

    await fireEvent.click(screen.getByRole('button', { name: 'Exit compare' }));
    await waitFor(() => expect(screen.queryByRole('heading', { name: /^A: / })).toBeNull());
  });

  it('compares a version with the folder via Ctrl+click', async () => {
    const { api, byLabel, row } = await setup();
    const changes = vi.spyOn(api, 'changes');
    await fireEvent.click(row('Working checkout'), { ctrlKey: true });
    await waitFor(() =>
      expect(changes).toHaveBeenLastCalledWith(expect.any(Number), byLabel('Working checkout').id, {
        kind: 'workingTree',
      }),
    );
  });
});

describe('version management (F11)', () => {
  it('edits a label inline', async () => {
    const { api, row } = await setup();
    const setLabel = vi.spyOn(api, 'setLabel');
    await fireEvent.contextMenu(row('New logo'));
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Edit label' }));
    const input = screen.getByLabelText('Version label');
    await fireEvent.input(input, { target: { value: 'Red square logo' } });
    await fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() =>
      expect(setLabel).toHaveBeenCalledWith(expect.any(Number), 'Red square logo'),
    );
    expect(row('Red square logo')).toBeInTheDocument();
  });

  it('pins, and rolls back when the API rejects', async () => {
    const { api, row } = await setup();
    vi.spyOn(api, 'setPinned').mockRejectedValueOnce(new ApiError('db', 'busy'));
    await fireEvent.contextMenu(row('New logo'));
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Pin (never prune)' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not pin: busy (db)');
    expect(within(row('New logo')).queryByTitle('Never pruned by retention')).toBeNull();

    await fireEvent.contextMenu(row('New logo'));
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Pin (never prune)' }));
    await waitFor(() =>
      expect(within(row('New logo')).getByTitle('Never pruned by retention')).toBeInTheDocument(),
    );
  });

  it('warns before deleting a safety version, then deletes it', async () => {
    const { api, row } = await setup();
    const del = vi.spyOn(api, 'deleteVersion');
    await fireEvent.contextMenu(row('Before restore to Working checkout'));
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Delete…' }));
    const dialog = screen.getByRole('dialog');
    expect(within(dialog).getByText(/removes the way to undo/)).toBeInTheDocument();
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(del).toHaveBeenCalled());
    expect(() => row('Before restore to Working checkout')).toThrow();
  });
});

describe('hunk revert (F22)', () => {
  async function openCart() {
    const files = await screen.findByRole('listbox', { name: 'Changed files' });
    await fireEvent.click(within(files).getByRole('option', { name: /cart\.ts/ }));
    await screen.findAllByText(/^@@ /);
  }

  it('offers Revert only against the folder and calls the API', async () => {
    const { api, row } = await setup();
    await openCart();
    const revert = vi.spyOn(api, 'revertHunk');
    const buttons = await screen.findAllByRole('button', { name: 'Revert' });
    await fireEvent.click(buttons[0]!);
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Revert' }),
    );
    await waitFor(() =>
      expect(revert).toHaveBeenCalledWith(
        expect.any(Number),
        null,
        { kind: 'workingTree' },
        'src/cart.ts',
        0,
        expect.objectContaining({ ignoreWhitespace: false }),
      ),
    );
    await screen.findByText(/Reverted the change in src\/cart.ts/);

    // A stored version: no Revert.
    await fireEvent.click(row('Initial import'));
    const files = await screen.findByRole('listbox', { name: 'Changed files' });
    await fireEvent.click(within(files).getAllByRole('option')[0]!);
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Revert' })).toBeNull());
  });

  it('reports a stale diff and reloads', async () => {
    const { api } = await setup();
    await openCart();
    vi.spyOn(api, 'revertHunk').mockRejectedValueOnce(
      new ApiError('invalid_input', 'src/cart.ts changed since the diff was computed'),
    );
    const status = vi.spyOn(api, 'status');
    await fireEvent.click((await screen.findAllByRole('button', { name: 'Revert' }))[0]!);
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Revert' }),
    );
    await screen.findByText('File changed since the diff was shown — refreshed.');
    expect(status).toHaveBeenCalled();
  });
});
