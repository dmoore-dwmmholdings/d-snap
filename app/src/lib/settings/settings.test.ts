// Chain J (DSNA-47): settings validation and saving.
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import { createMockApi } from '../api';
import { AppStore, storeContext } from '../stores/app.svelte';
import { validate, type Form } from './form';
import SettingsScreen from './SettingsScreen.svelte';

const ok: Form = {
  sizeCapMb: '50',
  retentionKeep: '200',
  extraIgnore: '',
  respectGitignore: true,
  autoKind: 'off',
  everyMinutes: '10',
  idleSeconds: '60',
};

describe('validate', () => {
  it('accepts sane values and rejects the rest', () => {
    expect(validate(ok)).toEqual({});
    expect(validate({ ...ok, sizeCapMb: '0' }).sizeCapMb).toMatch(/between 1 and/);
    expect(validate({ ...ok, retentionKeep: '1.5' }).retentionKeep).toMatch(/whole number/);
    expect(validate({ ...ok, autoKind: 'every', everyMinutes: '0' }).everyMinutes).toBeDefined();
    expect(validate({ ...ok, autoKind: 'afterIdle', idleSeconds: '2' }).idleSeconds).toBeDefined();
    // Fields of an unselected mode are not checked.
    expect(validate({ ...ok, everyMinutes: 'x' })).toEqual({});
  });
});

async function setup() {
  const api = createMockApi();
  const store = new AppStore(api);
  await store.loadProjects();
  const shop = store.projects.find((p) => p.name === 'web-shop')!;
  await store.selectProject(shop.id);
  store.view = 'settings';
  render(SettingsScreen, { context: storeContext(store) });
  await screen.findByText(/Data folder/);
  return { api, store, shop };
}

describe('SettingsScreen', () => {
  it('saves global and project settings in the right shapes', async () => {
    const { api, shop } = await setup();
    const setGlobal = vi.spyOn(api, 'setGlobalSettings');
    const setProject = vi.spyOn(api, 'setProjectSettings');
    await fireEvent.input(screen.getByLabelText(/Skip files larger than/), {
      target: { value: '100' },
    });
    await fireEvent.input(screen.getByRole('textbox', { name: /Also ignore/ }), {
      target: { value: '*.tmp\n\n  build/  \n' },
    });
    await fireEvent.click(screen.getByLabelText(/Every/));
    await fireEvent.input(screen.getByLabelText('Minutes'), { target: { value: '15' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(setGlobal).toHaveBeenCalledWith({
        sizeCapBytes: 100 * 1024 * 1024,
        retentionKeep: 200,
      }),
    );
    expect(setProject).toHaveBeenCalledWith(shop.id, {
      extraIgnore: ['*.tmp', 'build/'],
      respectGitignore: true,
      autoSnapshot: { kind: 'every', secs: 900 },
    });
  });

  it('blocks saving invalid values', async () => {
    await setup();
    await fireEvent.input(screen.getByLabelText(/Keep the last/), { target: { value: '0' } });
    expect(await screen.findByText(/between 1 and 100000/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
  });

  it('asks before leaving with unsaved edits', async () => {
    const { store } = await setup();
    await fireEvent.input(screen.getByLabelText(/Keep the last/), { target: { value: '50' } });
    store.setView('main');
    expect(store.view).toBe('settings');
    const dialog = await screen.findByRole('dialog', { name: 'Discard unsaved settings?' });
    await fireEvent.click(dialog.querySelector('[data-confirm]')!);
    expect(store.view).toBe('main');
  });

  it('shows the hooks snippet with --hook', async () => {
    await setup();
    expect(screen.getByText(/dsnap snap --hook/)).toBeInTheDocument();
  });
});
