<script lang="ts">
  import { onMount } from 'svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import {
    getConfig,
    setScannedRoot,
    defaultScannedRoot,
    createCourse,
    scanLibrary,
    renameCourse,
    pickDirectory,
    type CourseEntry
  } from '$lib/api';

  let scannedRoot = $state<string | null>(null);
  let entries = $state<CourseEntry[]>([]);
  let suggestedDefault = $state<string | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let renamingFolder = $state<string | null>(null);
  let renameDraft = $state('');
  let renameInputEl = $state<HTMLInputElement | null>(null);
  let newCourseOpen = $state(false);
  let newCourseDraft = $state('');
  let newCourseInputEl = $state<HTMLInputElement | null>(null);

  $effect(() => {
    if (renamingFolder && renameInputEl) {
      renameInputEl.focus();
      renameInputEl.select();
    }
  });

  $effect(() => {
    if (newCourseOpen && newCourseInputEl) {
      newCourseInputEl.focus();
      newCourseInputEl.select();
    }
  });

  async function refresh() {
    if (!scannedRoot) return;
    try {
      entries = await scanLibrary(scannedRoot);
    } catch (e) {
      error = String(e);
    }
  }

  async function chooseRoot() {
    error = null;
    const picked = await pickDirectory(suggestedDefault);
    if (!picked) return;
    try {
      await setScannedRoot(picked);
      scannedRoot = picked;
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  function openNewCourse() {
    newCourseDraft = 'Untitled Course';
    newCourseOpen = true;
  }

  function cancelNewCourse() {
    newCourseOpen = false;
    newCourseDraft = '';
  }

  async function commitNewCourse() {
    if (!scannedRoot || busy) return;
    const title = newCourseDraft.trim();
    if (!title) {
      cancelNewCourse();
      return;
    }
    busy = true;
    error = null;
    try {
      await createCourse(scannedRoot, title);
      newCourseOpen = false;
      newCourseDraft = '';
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function startRename(entry: CourseEntry) {
    renamingFolder = entry.folder;
    renameDraft = entry.title;
  }

  async function commitRename(entry: CourseEntry) {
    const next = renameDraft.trim();
    if (!next || next === entry.title) {
      renamingFolder = null;
      return;
    }
    busy = true;
    error = null;
    try {
      await renameCourse(entry.folder, next);
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
      renamingFolder = null;
    }
  }

  function fmtDate(ms: number) {
    if (!ms) return '';
    return new Date(ms).toLocaleString();
  }

  let unlistenFocus: (() => void) | null = null;

  onMount(() => {
    void (async () => {
      try {
        const [cfg, fallback] = await Promise.all([getConfig(), defaultScannedRoot()]);
        suggestedDefault = fallback;
        scannedRoot = cfg.scannedRoot;
        if (scannedRoot) await refresh();
      } catch (e) {
        error = String(e);
      }

      const win = getCurrentWindow();
      unlistenFocus = await win.onFocusChanged(({ payload: focused }) => {
        if (focused) refresh();
      });
    })();

    return () => {
      unlistenFocus?.();
    };
  });
</script>

<main>
  <header>
    <h1>Courseforge</h1>
    {#if scannedRoot}
      <div class="root">
        <span class="label">Scanning</span>
        <code>{scannedRoot}</code>
        <button class="link" onclick={chooseRoot}>Change…</button>
      </div>
    {/if}
  </header>

  {#if error}
    <div class="error" role="alert">{error}</div>
  {/if}

  {#if !scannedRoot}
    <section class="first-launch">
      <h2>Welcome</h2>
      <p>
        Choose a folder to keep your Courses in. Courseforge will look here for any
        folder containing a <code>course.json</code>.
      </p>
      {#if suggestedDefault}
        <p class="hint">Suggested: <code>{suggestedDefault}</code></p>
      {/if}
      <button class="primary" onclick={chooseRoot}>Choose folder…</button>
    </section>
  {:else}
    <section class="toolbar">
      {#if newCourseOpen}
        <form
          class="new-course-form"
          onsubmit={(e) => {
            e.preventDefault();
            void commitNewCourse();
          }}
        >
          <input
            bind:this={newCourseInputEl}
            bind:value={newCourseDraft}
            placeholder="Course title"
            onkeydown={(e) => {
              if (e.key === 'Escape') cancelNewCourse();
            }}
          />
          <button class="primary" type="submit" disabled={busy || !newCourseDraft.trim()}>
            Create
          </button>
          <button type="button" class="ghost" onclick={cancelNewCourse} disabled={busy}>
            Cancel
          </button>
        </form>
      {:else}
        <button class="primary" onclick={openNewCourse} disabled={busy}>
          + New Course
        </button>
      {/if}
    </section>

    {#if entries.length === 0}
      <section class="empty">
        <p>No Courses yet. Click <strong>New Course</strong> to create your first one.</p>
      </section>
    {:else}
      <table class="library">
        <thead>
          <tr>
            <th>Title</th>
            <th>Last modified</th>
            <th>Videos</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each entries as entry (entry.folder)}
            <tr>
              <td>
                {#if renamingFolder === entry.folder}
                  <input
                    bind:this={renameInputEl}
                    bind:value={renameDraft}
                    onblur={() => commitRename(entry)}
                    onkeydown={(e) => {
                      if (e.key === 'Enter') commitRename(entry);
                      else if (e.key === 'Escape') renamingFolder = null;
                    }}
                  />
                {:else}
                  <button class="title-btn" onclick={() => startRename(entry)} title={entry.folder}>
                    {entry.title}
                  </button>
                {/if}
              </td>
              <td class="mtime">{fmtDate(entry.modified_ms)}</td>
              <td class="count">{entry.video_count}</td>
              <td class="actions">
                <button class="link" onclick={() => startRename(entry)}>Rename</button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/if}
</main>

<style>
  :global(body) {
    margin: 0;
    font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Text', sans-serif;
    background: #fafafa;
    color: #111;
  }
  main {
    max-width: 920px;
    margin: 0 auto;
    padding: 2rem 1.5rem;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 1rem;
    margin-bottom: 1.5rem;
  }
  h1 { font-size: 1.5rem; margin: 0; }
  .root {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.85rem;
    color: #555;
  }
  .root .label { color: #888; }
  code { font-family: 'SF Mono', Menlo, monospace; font-size: 0.85em; background: #f0f0f0; padding: 1px 5px; border-radius: 3px; }
  .first-launch {
    background: white;
    padding: 2rem;
    border-radius: 10px;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04);
    text-align: center;
  }
  .first-launch h2 { margin-top: 0; }
  .hint { color: #666; }
  .toolbar { margin-bottom: 1rem; }
  button.primary {
    background: #0066ff;
    color: white;
    border: none;
    padding: 0.5rem 1rem;
    border-radius: 6px;
    font-size: 0.9rem;
    cursor: pointer;
  }
  button.primary:disabled { opacity: 0.5; cursor: not-allowed; }
  button.ghost {
    background: white;
    color: #333;
    border: 1px solid #ccc;
    padding: 0.5rem 1rem;
    border-radius: 6px;
    font-size: 0.9rem;
    cursor: pointer;
  }
  button.ghost:disabled { opacity: 0.5; cursor: not-allowed; }
  .new-course-form {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }
  .new-course-form input {
    flex: 1;
    max-width: 320px;
    font-size: 1rem;
    padding: 0.45rem 0.6rem;
  }
  button.link {
    background: none;
    border: none;
    color: #0066ff;
    cursor: pointer;
    padding: 0;
    font-size: 0.85rem;
  }
  button.title-btn {
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: #111;
    font-size: 1rem;
    text-align: left;
  }
  button.title-btn:hover { text-decoration: underline; }
  .empty {
    background: white;
    padding: 3rem 1rem;
    border-radius: 10px;
    text-align: center;
    color: #666;
  }
  table.library {
    width: 100%;
    background: white;
    border-radius: 10px;
    overflow: hidden;
    border-collapse: collapse;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04);
  }
  table.library th, table.library td {
    text-align: left;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid #eee;
    font-size: 0.9rem;
  }
  table.library th { color: #888; font-weight: 500; font-size: 0.75rem; text-transform: uppercase; }
  table.library tr:last-child td { border-bottom: none; }
  .mtime { color: #666; }
  .count { color: #444; }
  .actions { text-align: right; }
  .error {
    background: #ffe6e6;
    color: #b00020;
    padding: 0.75rem 1rem;
    border-radius: 6px;
    margin-bottom: 1rem;
    font-size: 0.85rem;
  }
  input {
    font-size: 1rem;
    padding: 2px 6px;
    border: 1px solid #ccc;
    border-radius: 4px;
    width: 100%;
    box-sizing: border-box;
  }
</style>
