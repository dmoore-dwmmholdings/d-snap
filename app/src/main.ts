import { mount } from 'svelte';
import './lib/theme.css';
import App from './App.svelte';
import { api } from './lib/api';
import { windowKind } from './lib/desktop';
import TrayView from './lib/tray/TrayView.svelte';

const target = document.getElementById('app');
if (!target) throw new Error('missing #app element');

// The tray popover loads the same bundle with `?view=tray` (DSNA-128).
export default windowKind() === 'tray'
  ? mount(TrayView, { target, props: { api } })
  : mount(App, { target });
