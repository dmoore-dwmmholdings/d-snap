import { render, screen } from '@testing-library/svelte';
import App from './App.svelte';

describe('App', () => {
  it('renders the app shell', () => {
    render(App);
    expect(screen.getByRole('heading', { name: 'D-Snap' })).toBeInTheDocument();
    expect(screen.getByText('No project selected.')).toBeInTheDocument();
  });
});
