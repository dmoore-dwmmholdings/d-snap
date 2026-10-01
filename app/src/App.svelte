<script lang="ts">
  const appName = 'D-Snap';
</script>

<main class="shell">
  <aside class="sidebar" aria-label="Projects">
    <h1 class="brand">{appName}</h1>
  </aside>
  <section class="content">
    <p class="empty">No project selected.</p>
  </section>
</main>

<style>
  .shell {
    display: grid;
    grid-template-columns: 240px 1fr;
    height: 100vh;
  }
  .sidebar {
    background: var(--surface-1);
    border-right: 1px solid var(--border);
    padding: var(--space-3);
  }
  .brand {
    font-size: var(--text-lg);
    font-weight: 600;
    margin: 0;
  }
  .content {
    display: grid;
    place-items: center;
  }
  .empty {
    color: var(--text-muted);
  }
</style>
