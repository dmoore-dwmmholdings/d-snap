// Chain R (DSNA-73): backend events update the views live.
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import App from '../../App.svelte';
import { createMockApi } from '../api';

async function setup() {
  const api = createMockApi({ now: () => Date.UTC(2026, 9, 1, 12) });
  render(App, { props: { api } });
  await fireEvent.click(await screen.findByRole('option', { name: /^rust-tool/ }));
  await screen.findByRole('heading', { level: 2, name: 'rust-tool' });
  const p = (await api.listProjects()).find((x) => x.name === 'rust-tool')!;
  return { api, p };
}

describe('live updates', () => {
  it('a folder change updates the badge, Unsaved row and Snapshot button', async () => {
    const { api, p } = await setup();
    const sidebar = screen.getByRole('listbox', { name: 'Projects' });
    const row = within(sidebar).getByRole('option', { name: /^rust-tool/ });
    expect(within(row).queryByLabelText(/changed/)).toBeNull();

    api.mockWriteFile(p.id, 'src/new.rs', 'x');
    await waitFor(() => expect(within(row).getByLabelText('1 changed')).toBeInTheDocument());
    expect(screen.getByRole('option', { name: /Unsaved changes/ })).toHaveTextContent(
      '1 file changed',
    );
    expect(screen.getByRole('button', { name: 'Snapshot' })).toBeEnabled();
  });

  it('a CLI snapshot appears in the list and keeps the selection', async () => {
    const { api, p } = await setup();
    const versions = screen.getByRole('listbox', { name: 'Versions' });
    const before = within(versions).getAllByRole('option').length;
    await fireEvent.click(within(versions).getAllByRole('option')[3]!);
    const selected = within(versions).getByRole('option', { selected: true }).id;

    api.mockWriteFile(p.id, 'src/agent.rs', 'fn agent() {}');
    api.mockCliSnapshot(p.id, 'after agent turn');
    await waitFor(() => expect(within(versions).getAllByRole('option')).toHaveLength(before + 1));
    expect(within(versions).getByRole('option', { selected: true }).id).toBe(selected);
  });

  it('reloads the project list when it changes elsewhere, and on focus', async () => {
    const { api } = await setup();
    const list = vi.spyOn(api, 'listProjects');
    api.mockEmitProjectsChanged();
    await waitFor(() => expect(list).toHaveBeenCalledTimes(1));
    window.dispatchEvent(new Event('focus'));
    await waitFor(() => expect(list).toHaveBeenCalledTimes(2));
  });
});
