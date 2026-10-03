// The "Turn review" window against the mock Api (DSNA-124..127).
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import App from './App.svelte';
import { createMockApi, type MockApi, type Project } from './lib/api';

async function setup(): Promise<{ api: MockApi; shop: Project }> {
  const api = createMockApi();
  const shop = (await api.listProjects()).find((p) => p.name === 'web-shop')!;
  render(App, { props: { api } });
  // Opens on the folder with a turn waiting, on that turn.
  await screen.findByRole('heading', { name: /Add a payment step after checkout/ });
  return { api, shop };
}

const history = () => screen.getByRole('complementary', { name: 'History' });

describe('App', () => {
  it('opens on the turn waiting for review with its files', async () => {
    await setup();
    expect(screen.getByRole('button', { name: 'Keep all' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Undo the whole turn' })).toBeInTheDocument();
    expect(
      await screen.findByText(
        'You changed this file after the turn. Undoing it would discard those edits.',
      ),
    ).toBeInTheDocument();
    const nav = screen.getByRole('navigation', { name: 'Folders' });
    expect(within(nav).getByRole('button', { name: /^web-shop/ })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    expect(within(nav).getByLabelText('1 waiting for review')).toBeInTheDocument();
    expect(within(history()).getByText(/Waiting for review/)).toBeInTheDocument();
  });

  it('keeps every file and marks the turn reviewed', async () => {
    const { api, shop } = await setup();
    await fireEvent.click(screen.getByRole('button', { name: 'Keep all' }));
    await screen.findByText('You kept everything from this turn.');
    const [turn] = await api.listTurns(shop.id);
    expect(turn!.state).toBe('reviewed');
    expect(screen.queryByLabelText('1 waiting for review')).not.toBeInTheDocument();
  });

  it('undoes the whole turn, leaving files changed since unless asked', async () => {
    const { api, shop } = await setup();
    await fireEvent.click(screen.getByRole('button', { name: 'Undo the whole turn' }));
    const d = screen.getByRole('dialog', { name: 'Undo Turn 5?' });
    expect(within(d).getByText(/changed again after this turn/)).toBeInTheDocument();
    expect(within(d).getByText('left alone')).toBeInTheDocument();
    await fireEvent.click(within(d).getByRole('button', { name: 'Undo turn' }));
    await screen.findByText(/Turn 5 is undone/);
    const files = await api.turnFiles((await api.listTurns(shop.id))[0]!.id);
    const payment = files.find((f) => f.change.path === 'src/payment.ts')!;
    expect(payment.decision).toBeNull();
    expect(files.some((f) => f.decision === 'undone')).toBe(true);
    expect((await api.listVersions(shop.id))[0]!.label).toBe('Before undoing Turn 5');
  });

  it('keeps one file and opens the next', async () => {
    await setup();
    const keep = await screen.findAllByRole('button', { name: 'Keep' });
    await fireEvent.click(keep[0]!);
    await screen.findByText('Kept');
    expect(screen.getByRole('button', { name: 'Keep the rest' })).toBeInTheDocument();
  });

  it('shows a checkpoint and asks before restoring the folder', async () => {
    await setup();
    await fireEvent.click(within(history()).getByRole('button', { name: /Checkpoint.*New logo/ }));
    await screen.findByRole('heading', { name: '“New logo”' });
    await fireEvent.click(screen.getByRole('button', { name: 'Restore folder to this…' }));
    const d = await screen.findByRole('dialog', { name: /Restore folder to “New logo”/ });
    await fireEvent.click(within(d).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('finds a file and shows its history', async () => {
    await setup();
    const box = screen.getByRole('combobox');
    await fireEvent.focus(box);
    await fireEvent.input(box, { target: { value: 'cart' } });
    await screen.findByRole('option', { name: /cart\.ts/ });
    await fireEvent.keyDown(box, { key: 'Enter' });
    await screen.findByRole('heading', { name: 'src/cart.ts' });
    expect(
      screen.getAllByRole('button', { name: 'Bring back this version' }).length,
    ).toBeGreaterThan(0);
  });

  it('takes a snapshot of the unsaved changes', async () => {
    const { api, shop } = await setup();
    const before = (await api.listVersions(shop.id)).length;
    await fireEvent.click(within(history()).getByRole('button', { name: /Not snapshotted yet/ }));
    await screen.findByRole('heading', { name: /not snapshotted yet/ });
    await fireEvent.click(screen.getByRole('button', { name: 'Take a snapshot' }));
    const d = screen.getByRole('dialog', { name: /Take a snapshot of web-shop/ });
    await fireEvent.input(within(d).getByRole('textbox'), { target: { value: 'Before lunch' } });
    await fireEvent.click(within(d).getByRole('button', { name: 'Save snapshot' }));
    await screen.findByRole('heading', { name: '“Before lunch”' });
    expect((await api.listVersions(shop.id)).length).toBe(before + 1);
  });

  it('shows a missing folder and lets you browse its snapshots', async () => {
    await setup();
    const nav = screen.getByRole('navigation', { name: 'Folders' });
    await fireEvent.click(within(nav).getByRole('button', { name: /notes/ }));
    await screen.findByRole('heading', { name: /We can’t find notes/ });
    await fireEvent.click(screen.getByRole('button', { name: 'Browse its snapshots' }));
    await waitFor(() =>
      expect(
        screen.queryByRole('heading', { name: /We can’t find notes/ }),
      ).not.toBeInTheDocument(),
    );
    expect(screen.getByRole('button', { name: 'Restore folder to this…' })).toBeDisabled();
  });

  it('opens Settings and comes back', async () => {
    await setup();
    await fireEvent.click(screen.getByRole('button', { name: 'Settings' }));
    await screen.findByRole('heading', { name: 'Settings' });
    expect(await screen.findByLabelText(/Keep d-snap running in the menu bar/)).toBeChecked();
    await fireEvent.click(screen.getByRole('button', { name: 'Done' }));
    await screen.findByRole('heading', { name: /Add a payment step/ });
  });

  it('records a new agent turn from the hooks', async () => {
    const { api } = await setup();
    const tool = (await api.listProjects()).find((p) => p.name === 'rust-tool')!;
    await fireEvent.click(screen.getByRole('button', { name: /rust-tool/ }));
    api.mockBeginTurn(tool.id, 'Rename the crate to dsnap-tool');
    api.mockWriteFile(tool.id, 'Cargo.toml', '[package]\nname = "dsnap-tool"\n');
    api.mockEndTurn(tool.id);
    await fireEvent.click(
      await within(history()).findByRole('button', { name: /Turn \d+.*Rename the crate/ }),
    );
    await screen.findByRole('heading', { name: /Rename the crate to dsnap-tool/ });
    expect(screen.getByRole('button', { name: 'Keep all' })).toBeInTheDocument();
  });
});
