<script lang="ts">
  import { onMount } from 'svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { convertFileSrc } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import {
    getWindowCourseFolder,
    readCourse,
    addModule,
    renameModule,
    reorderModules,
    deleteModule,
    addVideo,
    renameVideo,
    reorderVideosInModule,
    deleteVideo,
    moveVideoToModule,
    addWorkflowState,
    renameWorkflowState,
    reorderWorkflowStates,
    removeWorkflowState,
    setVideoState,
    recordingPreflight,
    openSettingsPane,
    startRecording,
    pauseRecording,
    resumeRecording,
    stopRecording,
    keepSegment,
    discardSegment,
    listActiveSessions,
    hasActiveRecording,
    listSegments,
    scanOrphanSegments,
    importOrphanSegment,
    discardOrphanSegment,
    listTranscriptionJobs,
    retryTranscription,
    getTranscript,
    type Course,
    type Module,
    type Video,
    type WorkflowState,
    type PermissionsSnapshot,
    type SessionSnapshot,
    type Segment,
    type OrphanSegment,
    type CaptureSources,
    type Transcript,
    type TranscriptionJob,
    formatError
  } from '$lib/api';

  let folder = $state<string | null>(null);
  let course = $state<Course | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);

  // Tree vs Board.
  let view = $state<'tree' | 'board'>('tree');

  // Workflow state editor (modal-ish inline panel).
  let editingStates = $state(false);
  let newStateDraft = $state('');
  let editingStateId = $state<string | null>(null);
  let editStateDraft = $state('');

  // Drag state for the board view. Tracked at module level so we can
  // highlight the drop target column.
  let draggingVideoId = $state<string | null>(null);
  let dropTargetStateId = $state<string | null>(null);

  // Inline edit state
  let editing = $state<{ kind: 'module' | 'video'; id: string } | null>(null);
  let editDraft = $state('');
  let editInputEl = $state<HTMLInputElement | null>(null);

  // New-row state per module
  let newVideoModuleId = $state<string | null>(null);
  let newVideoDraft = $state('');
  let newVideoInputEl = $state<HTMLInputElement | null>(null);

  // New module draft
  let newModuleOpen = $state(false);
  let newModuleDraft = $state('');
  let newModuleInputEl = $state<HTMLInputElement | null>(null);

  $effect(() => {
    if (editing && editInputEl) {
      editInputEl.focus();
      editInputEl.select();
    }
  });
  $effect(() => {
    if (newVideoModuleId && newVideoInputEl) {
      newVideoInputEl.focus();
      newVideoInputEl.select();
    }
  });
  $effect(() => {
    if (newModuleOpen && newModuleInputEl) {
      newModuleInputEl.focus();
      newModuleInputEl.select();
    }
  });

  async function refresh() {
    if (!folder) return;
    try {
      course = await readCourse(folder);
    } catch (e) {
      error = formatError(e);
    }
  }

  function videoById(id: string): Video | undefined {
    return course?.videos.find((v) => v.id === id);
  }

  async function withBusy<T>(fn: () => Promise<T>): Promise<T | undefined> {
    if (busy) return;
    busy = true;
    error = null;
    try {
      return await fn();
    } catch (e) {
      error = formatError(e);
    } finally {
      busy = false;
    }
  }

  function openNewModule() {
    newModuleDraft = 'New Module';
    newModuleOpen = true;
  }
  function cancelNewModule() {
    newModuleOpen = false;
    newModuleDraft = '';
  }
  async function commitNewModule() {
    const title = newModuleDraft.trim();
    if (!folder || !title) {
      cancelNewModule();
      return;
    }
    await withBusy(async () => {
      await addModule(folder!, title);
      newModuleOpen = false;
      newModuleDraft = '';
      await refresh();
    });
  }

  function startEdit(kind: 'module' | 'video', id: string, currentTitle: string) {
    editing = { kind, id };
    editDraft = currentTitle;
  }
  function cancelEdit() {
    editing = null;
    editDraft = '';
  }
  async function commitEdit() {
    if (!editing || !folder) return cancelEdit();
    const next = editDraft.trim();
    const { kind, id } = editing;
    const currentTitle =
      kind === 'module'
        ? course?.modules.find((m) => m.id === id)?.title
        : videoById(id)?.title;
    if (!next || next === currentTitle) return cancelEdit();
    await withBusy(async () => {
      if (kind === 'module') await renameModule(folder!, id, next);
      else await renameVideo(folder!, id, next);
      editing = null;
      await refresh();
    });
  }

  async function deleteModuleAt(m: Module) {
    if (!folder) return;
    const childCount = m.videoIds.length;
    const msg =
      childCount === 0
        ? `Delete module "${m.title}"?`
        : `Delete module "${m.title}" and its ${childCount} video${childCount === 1 ? '' : 's'}?`;
    if (!confirm(msg)) return;
    await withBusy(async () => {
      await deleteModule(folder!, m.id);
      await refresh();
    });
  }

  async function deleteVideoAt(v: Video) {
    if (!folder) return;
    if (!confirm(`Delete video "${v.title}"?`)) return;
    await withBusy(async () => {
      await deleteVideo(folder!, v.id);
      await refresh();
    });
  }

  function openNewVideoForm(moduleId: string) {
    newVideoModuleId = moduleId;
    newVideoDraft = 'New Video';
  }
  function cancelNewVideo() {
    newVideoModuleId = null;
    newVideoDraft = '';
  }
  async function commitNewVideo() {
    const title = newVideoDraft.trim();
    const mid = newVideoModuleId;
    if (!folder || !mid || !title) return cancelNewVideo();
    await withBusy(async () => {
      await addVideo(folder!, mid, title);
      newVideoModuleId = null;
      newVideoDraft = '';
      await refresh();
    });
  }

  async function moveModule(m: Module, delta: -1 | 1) {
    if (!course || !folder) return;
    const ids = course.modules.map((x) => x.id);
    const i = ids.indexOf(m.id);
    const j = i + delta;
    if (i < 0 || j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    await withBusy(async () => {
      await reorderModules(folder!, ids);
      await refresh();
    });
  }

  async function moveVideoInModule(m: Module, v: Video, delta: -1 | 1) {
    if (!folder) return;
    const ids = [...m.videoIds];
    const i = ids.indexOf(v.id);
    const j = i + delta;
    if (i < 0 || j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    await withBusy(async () => {
      await reorderVideosInModule(folder!, m.id, ids);
      await refresh();
    });
  }

  async function moveVideoToOtherModule(v: Video, targetModuleId: string) {
    if (!folder) return;
    await withBusy(async () => {
      const dst = course?.modules.find((m) => m.id === targetModuleId);
      const index = dst ? dst.videoIds.length : 0;
      await moveVideoToModule(folder!, v.id, targetModuleId, index);
      await refresh();
    });
  }

  function stateById(id: string | null): WorkflowState | undefined {
    if (!id) return undefined;
    return course?.workflowStates.find((s) => s.id === id);
  }

  function videosInState(stateId: string): Video[] {
    if (!course) return [];
    return course.videos.filter((v) => (v.stateId ?? course!.workflowStates[0]?.id) === stateId);
  }

  function moduleTitleForVideo(videoId: string): string | undefined {
    return course?.modules.find((m) => m.videoIds.includes(videoId))?.title;
  }

  async function transitionVideo(videoId: string, newStateId: string) {
    if (!folder) return;
    const v = course?.videos.find((x) => x.id === videoId);
    if (!v || v.stateId === newStateId) return;
    await withBusy(async () => {
      await setVideoState(folder!, videoId, newStateId);
      await refresh();
    });
  }

  // Board drag handlers — HTML5 native DnD, no extra deps.
  function onCardDragStart(e: DragEvent, videoId: string) {
    draggingVideoId = videoId;
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = 'move';
      // Some browsers require any data to be set for the drag to fire.
      e.dataTransfer.setData('text/plain', videoId);
    }
  }
  function onCardDragEnd() {
    draggingVideoId = null;
    dropTargetStateId = null;
  }
  function onColumnDragOver(e: DragEvent, stateId: string) {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
    dropTargetStateId = stateId;
  }
  function onColumnDragLeave(stateId: string) {
    if (dropTargetStateId === stateId) dropTargetStateId = null;
  }
  async function onColumnDrop(e: DragEvent, stateId: string) {
    e.preventDefault();
    const id = draggingVideoId ?? e.dataTransfer?.getData('text/plain') ?? '';
    draggingVideoId = null;
    dropTargetStateId = null;
    if (id) await transitionVideo(id, stateId);
  }

  // ----- Workflow state editor handlers -----
  function openStateEditor() {
    editingStates = true;
    newStateDraft = '';
    editingStateId = null;
    editStateDraft = '';
  }
  function closeStateEditor() {
    editingStates = false;
    editingStateId = null;
    editStateDraft = '';
    newStateDraft = '';
  }
  async function commitAddState() {
    if (!folder) return;
    const name = newStateDraft.trim();
    if (!name) return;
    await withBusy(async () => {
      await addWorkflowState(folder!, name);
      newStateDraft = '';
      await refresh();
    });
  }
  function startEditState(s: WorkflowState) {
    editingStateId = s.id;
    editStateDraft = s.name;
  }
  function cancelEditState() {
    editingStateId = null;
    editStateDraft = '';
  }
  async function commitEditState() {
    if (!folder || !editingStateId) return cancelEditState();
    const next = editStateDraft.trim();
    const current = course?.workflowStates.find((s) => s.id === editingStateId)?.name;
    if (!next || next === current) return cancelEditState();
    const id = editingStateId;
    await withBusy(async () => {
      await renameWorkflowState(folder!, id, next);
      editingStateId = null;
      await refresh();
    });
  }
  async function moveStateOrder(s: WorkflowState, delta: -1 | 1) {
    if (!course || !folder) return;
    const ids = course.workflowStates.map((x) => x.id);
    const i = ids.indexOf(s.id);
    const j = i + delta;
    if (i < 0 || j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    await withBusy(async () => {
      await reorderWorkflowStates(folder!, ids);
      await refresh();
    });
  }
  async function removeStateAt(s: WorkflowState) {
    if (!course || !folder) return;
    if (course.workflowStates.length <= 1) {
      alert('Cannot remove the last remaining workflow state.');
      return;
    }
    const affected = course.videos.filter((v) => v.stateId === s.id).length;
    const choices = course.workflowStates.filter((x) => x.id !== s.id);
    const promptMsg =
      affected === 0
        ? `Remove state "${s.name}"?\n\nNo videos are currently in this state.\n\nEnter the name of a state to move (irrelevant — leave default), then OK:`
        : `Remove state "${s.name}"?\n\n${affected} video${affected === 1 ? '' : 's'} will be moved to a state of your choice.\n\nEnter the name of the destination state:\n  ${choices.map((c) => c.name).join('\n  ')}`;
    const fallbackName = window.prompt(promptMsg, choices[0]?.name ?? '');
    if (fallbackName == null) return;
    const fallback = choices.find(
      (c) => c.name.toLowerCase() === fallbackName.trim().toLowerCase()
    );
    if (!fallback) {
      alert(`No state named "${fallbackName}". Aborted.`);
      return;
    }
    await withBusy(async () => {
      await removeWorkflowState(folder!, s.id, fallback.id);
      await refresh();
    });
  }

  // ------------------------------------------------------------
  // Recording state — per-video and per-course (orphans, sessions)
  // ------------------------------------------------------------
  let permissions = $state<PermissionsSnapshot | null>(null);
  let permsBlockingVideoId = $state<string | null>(null); // video that triggered a "perms missing" banner
  let sessionsByVideo = $state<Record<string, SessionSnapshot>>({});
  let segmentsByVideo = $state<Record<string, Segment[]>>({});
  let orphans = $state<OrphanSegment[]>([]);
  let recordingSources = $state<CaptureSources>({
    microphone: true,
    systemAudio: false,
    webcam: false
  });
  // Elapsed-seconds tick so the recording panel timer updates without us
  // pushing per-session timers from Rust.
  let nowTick = $state(0);
  let sessionStartedAt = $state<Record<string, number>>({});

  // The recording panel renders separately to the row; remember which row to
  // anchor it to.
  function sessionForVideo(videoId: string): SessionSnapshot | undefined {
    return sessionsByVideo[videoId];
  }

  function permissionsSatisfied(p: PermissionsSnapshot | null, s: CaptureSources): boolean {
    if (!p) return false;
    if (p.screenRecording !== 'granted') return false;
    if (s.microphone && p.microphone !== 'granted' && p.microphone !== 'notDetermined') return false;
    if (s.webcam && p.camera !== 'granted' && p.camera !== 'notDetermined') return false;
    return true;
  }

  async function refreshSessions() {
    try {
      const list = await listActiveSessions();
      const next: Record<string, SessionSnapshot> = {};
      for (const s of list) next[s.videoId] = s;
      sessionsByVideo = next;
    } catch (e) {
      // Non-fatal — session listing is best-effort.
      console.warn('listActiveSessions failed', e);
    }
  }

  async function refreshSegments(videoId: string) {
    if (!folder) return;
    try {
      segmentsByVideo = { ...segmentsByVideo, [videoId]: await listSegments(folder, videoId) };
    } catch (e) {
      console.warn('listSegments failed', e);
    }
  }

  async function refreshAllSegments() {
    if (!folder || !course) return;
    const map: Record<string, Segment[]> = {};
    await Promise.all(
      course.videos.map(async (v) => {
        try {
          map[v.id] = await listSegments(folder!, v.id);
        } catch {
          map[v.id] = [];
        }
      })
    );
    segmentsByVideo = map;
  }

  async function refreshOrphans() {
    if (!folder) return;
    try {
      orphans = await scanOrphanSegments(folder);
    } catch (e) {
      console.warn('scanOrphans failed', e);
    }
  }

  async function refreshPermissions() {
    try {
      permissions = await recordingPreflight();
    } catch (e) {
      console.warn('preflight failed', e);
    }
  }

  async function startRecordingForVideo(videoId: string) {
    if (!folder) return;
    await refreshPermissions();
    if (!permissionsSatisfied(permissions, recordingSources)) {
      permsBlockingVideoId = videoId;
      return;
    }
    permsBlockingVideoId = null;
    await withBusy(async () => {
      const snap = await startRecording(folder!, videoId, recordingSources);
      sessionsByVideo = { ...sessionsByVideo, [videoId]: snap };
      sessionStartedAt = { ...sessionStartedAt, [snap.id]: Date.now() };
    });
  }

  async function pauseFor(videoId: string) {
    const s = sessionForVideo(videoId);
    if (!s) return;
    await withBusy(async () => {
      const next = await pauseRecording(s.id);
      sessionsByVideo = { ...sessionsByVideo, [videoId]: next };
    });
  }

  async function resumeFor(videoId: string) {
    const s = sessionForVideo(videoId);
    if (!s) return;
    await withBusy(async () => {
      const next = await resumeRecording(s.id);
      sessionsByVideo = { ...sessionsByVideo, [videoId]: next };
    });
  }

  async function stopFor(videoId: string) {
    const s = sessionForVideo(videoId);
    if (!s) return;
    await withBusy(async () => {
      const next = await stopRecording(s.id);
      sessionsByVideo = { ...sessionsByVideo, [videoId]: next };
    });
  }

  async function keepFor(videoId: string) {
    const s = sessionForVideo(videoId);
    if (!s) return;
    await withBusy(async () => {
      await keepSegment(s.id);
      const copy = { ...sessionsByVideo };
      delete copy[videoId];
      sessionsByVideo = copy;
      await refreshSegments(videoId);
    });
  }

  async function discardFor(videoId: string) {
    const s = sessionForVideo(videoId);
    if (!s) return;
    const activeWarn =
      s.state === 'recording' || s.state === 'paused'
        ? 'Discard this in-progress recording? The capture so far will be lost.'
        : 'Discard this recording? The capture will be deleted.';
    if (!confirm(activeWarn)) return;
    await withBusy(async () => {
      await discardSegment(s.id);
      const copy = { ...sessionsByVideo };
      delete copy[videoId];
      sessionsByVideo = copy;
    });
  }

  async function importOrphan(o: OrphanSegment) {
    if (!folder) return;
    await withBusy(async () => {
      await importOrphanSegment(folder!, o.videoId, o.id);
      await Promise.all([refreshOrphans(), refreshSegments(o.videoId)]);
    });
  }

  async function dropOrphan(o: OrphanSegment) {
    if (!folder) return;
    if (!confirm('Discard this recovered recording? The file will be deleted.')) return;
    await withBusy(async () => {
      await discardOrphanSegment(folder!, o.videoId, o.id);
      await refreshOrphans();
    });
  }

  async function openPane(pane: 'screenRecording' | 'camera' | 'microphone') {
    try {
      await openSettingsPane(pane);
    } catch (e) {
      error = formatError(e);
    }
  }

  function elapsedSecondsFor(sessionId: string): number {
    const at = sessionStartedAt[sessionId];
    if (!at) return 0;
    // nowTick is referenced so $derived/effects re-evaluate each second.
    const _ = nowTick;
    return Math.max(0, Math.floor((Date.now() - at) / 1000));
  }

  function fmtClock(seconds: number): string {
    const m = Math.floor(seconds / 60).toString().padStart(2, '0');
    const s = (seconds % 60).toString().padStart(2, '0');
    return `${m}:${s}`;
  }

  function videoTitleForOrphan(o: OrphanSegment): string {
    return course?.videos.find((v) => v.id === o.videoId)?.title ?? o.videoId;
  }

  // Per-second tick while any session is in a non-terminal state.
  $effect(() => {
    const any = Object.values(sessionsByVideo).some(
      (s) => s.state === 'recording' || s.state === 'paused'
    );
    if (!any) return;
    const handle = setInterval(() => (nowTick = Date.now()), 1000);
    return () => clearInterval(handle);
  });

  // ------------------------------------------------------------
  // Transcription state — per-video jobs + the open video panel
  // ------------------------------------------------------------
  let jobsByVideo = $state<Record<string, TranscriptionJob>>({});
  let transcriptByVideo = $state<Record<string, Transcript | null>>({});
  let openVideoId = $state<string | null>(null);
  let videoEl = $state<HTMLVideoElement | null>(null);
  let activeWordIndex = $state<number>(-1);

  async function refreshJobs() {
    try {
      const list = await listTranscriptionJobs();
      const next: Record<string, TranscriptionJob> = {};
      for (const j of list) next[j.videoId] = j;
      jobsByVideo = next;
    } catch (e) {
      console.warn('listTranscriptionJobs failed', e);
    }
  }

  async function loadTranscript(videoId: string) {
    if (!folder) return;
    try {
      const t = await getTranscript(folder, videoId);
      transcriptByVideo = { ...transcriptByVideo, [videoId]: t };
    } catch (e) {
      console.warn('getTranscript failed', e);
    }
  }

  function applyJob(job: TranscriptionJob) {
    jobsByVideo = { ...jobsByVideo, [job.videoId]: job };
    if (job.status.kind === 'done') {
      // Transcript file just landed — pull it in so the open panel can render.
      void loadTranscript(job.videoId);
      // Also refresh segments in case Keep just transitioned us here.
      void refreshSegments(job.videoId);
    }
  }

  async function retryFor(videoId: string) {
    try {
      await retryTranscription(videoId);
      await refreshJobs();
    } catch (e) {
      error = formatError(e);
    }
  }

  async function toggleVideoPanel(videoId: string) {
    if (openVideoId === videoId) {
      openVideoId = null;
      activeWordIndex = -1;
      return;
    }
    openVideoId = videoId;
    activeWordIndex = -1;
    if (!(videoId in transcriptByVideo)) {
      await loadTranscript(videoId);
    }
  }

  function firstSegmentSrc(videoId: string): string | null {
    if (!folder) return null;
    const segs = segmentsByVideo[videoId] ?? [];
    if (segs.length === 0) return null;
    // segs[0].path is relative to the Course Folder — join then convert.
    const abs = `${folder}/${segs[0].path}`;
    return convertFileSrc(abs);
  }

  function onTimeUpdate(t: Transcript | null | undefined) {
    if (!t || !videoEl) return;
    const now = videoEl.currentTime;
    // Linear scan is fine for typical Video lengths; binary search if we ever
    // care about 30-minute talks with thousands of words.
    let idx = -1;
    for (let i = 0; i < t.words.length; i++) {
      if (now >= t.words[i].start && now < t.words[i].end) {
        idx = i;
        break;
      }
    }
    if (idx !== activeWordIndex) activeWordIndex = idx;
  }

  function jumpToWord(w: { start: number }) {
    if (!videoEl) return;
    videoEl.currentTime = w.start;
    void videoEl.play();
  }

  function jobLabel(job: TranscriptionJob | undefined): string | null {
    if (!job) return null;
    switch (job.status.kind) {
      case 'pending':
        return 'Transcribing…';
      case 'running':
        return `Transcribing ${Math.round(job.status.fraction * 100)}%`;
      case 'failed':
        return 'Transcription failed';
      case 'done':
        return 'Transcribed';
    }
  }

  onMount(() => {
    let unlistenClose: (() => void) | null = null;
    let unlistenJob: UnlistenFn | null = null;
    void (async () => {
      try {
        folder = await getWindowCourseFolder();
        if (!folder) {
          error = 'This window is not bound to a Course folder.';
          return;
        }
        await refresh();
        await Promise.all([
          refreshPermissions(),
          refreshSessions(),
          refreshOrphans(),
          refreshAllSegments(),
          refreshJobs()
        ]);

        // Push updates from the transcription worker keep the per-Video
        // status badges live without polling.
        try {
          unlistenJob = await listen<TranscriptionJob>('transcription-job', (e) => {
            applyJob(e.payload);
          });
        } catch (e) {
          console.warn('transcription-job listener wiring failed', e);
        }

        // Close-window guard: while any session is non-terminal, intercept
        // the close request and ask the user to confirm losing the take.
        try {
          const win = getCurrentWindow();
          const u = await win.onCloseRequested(async (event) => {
            if (await hasActiveRecording()) {
              const ok = confirm(
                'A recording is still in progress. Closing this window will stop and discard it. Continue?'
              );
              if (!ok) {
                event.preventDefault();
                return;
              }
              // User accepted — discard every active session before allowing
              // the close so we don't leave orphan ffmpegs behind.
              for (const s of Object.values(sessionsByVideo)) {
                try {
                  await discardSegment(s.id);
                } catch {
                  /* best effort */
                }
              }
            }
          });
          unlistenClose = u;
        } catch (e) {
          console.warn('onCloseRequested wiring failed', e);
        }
      } catch (e) {
        error = formatError(e);
      }
    })();
    return () => {
      if (unlistenClose) unlistenClose();
      if (unlistenJob) unlistenJob();
    };
  });
</script>

<main>
  <header>
    <h1>{course?.title ?? 'Loading…'}</h1>
    {#if folder}
      <code class="folder" title={folder}>{folder}</code>
    {/if}
  </header>

  {#if error}
    <div class="error" role="alert">{error}</div>
  {/if}

  {#if orphans.length > 0}
    <section class="orphan-banner" aria-label="Recovered recordings">
      <h2>Recovered recording{orphans.length === 1 ? '' : 's'}</h2>
      <p class="hint">
        {orphans.length === 1
          ? 'A previous recording session was interrupted.'
          : `${orphans.length} previous recording sessions were interrupted.`}
        The captured file{orphans.length === 1 ? '' : 's'} may still be usable.
      </p>
      <ul class="orphan-list">
        {#each orphans as o (`${o.videoId}:${o.id}`)}
          <li>
            <div class="orphan-meta">
              <strong>{videoTitleForOrphan(o)}</strong>
              <code title={o.path}>{o.path}</code>
            </div>
            <div class="actions">
              <button class="primary" disabled={busy} onclick={() => importOrphan(o)}>
                Import
              </button>
              <button class="ghost" disabled={busy} onclick={() => dropOrphan(o)}>
                Discard
              </button>
            </div>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if permsBlockingVideoId && permissions}
    {@const p = permissions}
    <section class="perms-banner" role="alert">
      <h2>Grant capture permissions to record</h2>
      <p class="hint">
        macOS requires you to allow Courseforge access in System Settings before it can capture.
      </p>
      <ul class="perms-list">
        {#if p.screenRecording !== 'granted'}
          <li>
            <span class="perm-name">Screen Recording</span>
            <span class="perm-status status-{p.screenRecording}">{p.screenRecording}</span>
            <button class="link" onclick={() => openPane('screenRecording')}>
              Open System Settings
            </button>
          </li>
        {/if}
        {#if recordingSources.microphone && p.microphone !== 'granted' && p.microphone !== 'notDetermined'}
          <li>
            <span class="perm-name">Microphone</span>
            <span class="perm-status status-{p.microphone}">{p.microphone}</span>
            <button class="link" onclick={() => openPane('microphone')}>
              Open System Settings
            </button>
          </li>
        {/if}
        {#if recordingSources.webcam && p.camera !== 'granted' && p.camera !== 'notDetermined'}
          <li>
            <span class="perm-name">Camera</span>
            <span class="perm-status status-{p.camera}">{p.camera}</span>
            <button class="link" onclick={() => openPane('camera')}>
              Open System Settings
            </button>
          </li>
        {/if}
      </ul>
      <div class="actions">
        <button
          class="primary"
          disabled={busy}
          onclick={async () => {
            const vid = permsBlockingVideoId;
            await refreshPermissions();
            if (vid && permissionsSatisfied(permissions, recordingSources)) {
              permsBlockingVideoId = null;
              await startRecordingForVideo(vid);
            }
          }}
        >
          Re-check &amp; Start
        </button>
        <button class="ghost" disabled={busy} onclick={() => (permsBlockingVideoId = null)}>
          Cancel
        </button>
      </div>
    </section>
  {/if}

  {#if course}
    <section class="view-toolbar">
      <div class="view-toggle" role="tablist" aria-label="View">
        <button
          role="tab"
          aria-selected={view === 'tree'}
          class:active={view === 'tree'}
          onclick={() => (view = 'tree')}
        >Tree</button>
        <button
          role="tab"
          aria-selected={view === 'board'}
          class:active={view === 'board'}
          onclick={() => (view = 'board')}
        >Board</button>
      </div>
      <button class="ghost" onclick={openStateEditor} disabled={busy}>Edit States…</button>
    </section>

    {#if editingStates}
      <section class="state-editor" aria-label="Workflow states">
        <header>
          <h2>Workflow States</h2>
          <button class="ghost" onclick={closeStateEditor} disabled={busy}>Done</button>
        </header>
        <ol class="states">
          {#each course.workflowStates as s, si (s.id)}
            <li class="state-row">
              <div class="title-area">
                {#if editingStateId === s.id}
                  <input
                    bind:value={editStateDraft}
                    onblur={commitEditState}
                    onkeydown={(e) => {
                      if (e.key === 'Enter') commitEditState();
                      else if (e.key === 'Escape') cancelEditState();
                    }}
                  />
                {:else}
                  <button class="title-btn" onclick={() => startEditState(s)}>{s.name}</button>
                {/if}
              </div>
              <div class="actions">
                <button class="icon" disabled={si === 0 || busy} onclick={() => moveStateOrder(s, -1)} title="Move up">↑</button>
                <button class="icon" disabled={si === course.workflowStates.length - 1 || busy} onclick={() => moveStateOrder(s, 1)} title="Move down">↓</button>
                <button class="link" disabled={busy} onclick={() => startEditState(s)}>Rename</button>
                <button
                  class="link danger"
                  disabled={busy || course.workflowStates.length <= 1}
                  onclick={() => removeStateAt(s)}
                >Remove</button>
              </div>
            </li>
          {/each}
        </ol>
        <form
          class="new-form"
          onsubmit={(e) => {
            e.preventDefault();
            void commitAddState();
          }}
        >
          <input
            bind:value={newStateDraft}
            placeholder="New state name"
          />
          <button class="primary" type="submit" disabled={busy || !newStateDraft.trim()}>
            Add State
          </button>
        </form>
      </section>
    {/if}

    {#if view === 'tree'}
    <section class="toolbar">
      {#if newModuleOpen}
        <form
          class="new-form"
          onsubmit={(e) => {
            e.preventDefault();
            void commitNewModule();
          }}
        >
          <input
            bind:this={newModuleInputEl}
            bind:value={newModuleDraft}
            placeholder="Module title"
            onkeydown={(e) => {
              if (e.key === 'Escape') cancelNewModule();
            }}
          />
          <button class="primary" type="submit" disabled={busy || !newModuleDraft.trim()}>
            Add
          </button>
          <button type="button" class="ghost" onclick={cancelNewModule} disabled={busy}>
            Cancel
          </button>
        </form>
      {:else}
        <button class="primary" onclick={openNewModule} disabled={busy}>+ Add Module</button>
      {/if}
    </section>

    {#if course.modules.length === 0}
      <section class="empty">
        <p>No Modules yet. Click <strong>Add Module</strong> to create your first one.</p>
      </section>
    {:else}
      <ol class="modules">
        {#each course.modules as m, mi (m.id)}
          <li class="module">
            <header class="module-header">
              <div class="title-area">
                {#if editing?.kind === 'module' && editing.id === m.id}
                  <input
                    bind:this={editInputEl}
                    bind:value={editDraft}
                    onblur={commitEdit}
                    onkeydown={(e) => {
                      if (e.key === 'Enter') commitEdit();
                      else if (e.key === 'Escape') cancelEdit();
                    }}
                  />
                {:else}
                  <button class="title-btn module-title" onclick={() => startEdit('module', m.id, m.title)}>
                    {m.title}
                  </button>
                {/if}
                <span class="count">{m.videoIds.length} video{m.videoIds.length === 1 ? '' : 's'}</span>
              </div>
              <div class="actions">
                <button class="icon" disabled={mi === 0 || busy} onclick={() => moveModule(m, -1)} title="Move up">↑</button>
                <button class="icon" disabled={mi === course.modules.length - 1 || busy} onclick={() => moveModule(m, 1)} title="Move down">↓</button>
                <button class="link" disabled={busy} onclick={() => startEdit('module', m.id, m.title)}>Rename</button>
                <button class="link danger" disabled={busy} onclick={() => deleteModuleAt(m)}>Delete</button>
              </div>
            </header>

            <ul class="videos">
              {#each m.videoIds as vid, vi (vid)}
                {@const v = videoById(vid)}
                {#if v}
                  {@const sess = sessionForVideo(v.id)}
                  {@const segs = segmentsByVideo[v.id] ?? []}
                  <li class="video">
                    <div class="title-area">
                      {#if editing?.kind === 'video' && editing.id === v.id}
                        <input
                          bind:this={editInputEl}
                          bind:value={editDraft}
                          onblur={commitEdit}
                          onkeydown={(e) => {
                            if (e.key === 'Enter') commitEdit();
                            else if (e.key === 'Escape') cancelEdit();
                          }}
                        />
                      {:else}
                        <button class="title-btn" onclick={() => startEdit('video', v.id, v.title)}>
                          {v.title}
                        </button>
                      {/if}
                      {#if stateById(v.stateId)}
                        <span class="state-badge" title="Workflow state">
                          {stateById(v.stateId)?.name}
                        </span>
                      {/if}
                      {#if segs.length > 0}
                        <span class="segments-badge" title="Recorded segments">
                          {segs.length} segment{segs.length === 1 ? '' : 's'}
                        </span>
                      {/if}
                      {#if jobLabel(jobsByVideo[v.id])}
                        {@const job = jobsByVideo[v.id]}
                        <span
                          class="trx-badge"
                          class:running={job?.status.kind === 'running' || job?.status.kind === 'pending'}
                          class:failed={job?.status.kind === 'failed'}
                          class:done={job?.status.kind === 'done'}
                          title={job?.status.kind === 'failed' ? job.status.message : ''}
                        >
                          {jobLabel(job)}
                        </span>
                      {/if}
                      {#if sess && (sess.state === 'recording' || sess.state === 'paused')}
                        <span
                          class="rec-badge"
                          class:paused={sess.state === 'paused'}
                          title={sess.state}
                        >
                          ● {sess.state === 'paused' ? 'PAUSED' : 'REC'}
                        </span>
                      {/if}
                    </div>
                    <div class="actions">
                      {#if !sess}
                        <button
                          class="link rec-btn"
                          disabled={busy || segs.length > 0}
                          title={segs.length > 0
                            ? 'This Video already has a Segment — multi-Segment Videos arrive in a later slice.'
                            : 'Start recording for this Video'}
                          onclick={() => startRecordingForVideo(v.id)}
                        >
                          ● Record
                        </button>
                      {/if}
                      {#if segs.length > 0}
                        <button
                          class="link"
                          onclick={() => toggleVideoPanel(v.id)}
                          title="Watch with transcript"
                        >
                          {openVideoId === v.id ? 'Hide' : 'Watch'}
                        </button>
                      {/if}
                      {#if jobsByVideo[v.id]?.status.kind === 'failed'}
                        <button
                          class="link"
                          onclick={() => retryFor(v.id)}
                          title={jobsByVideo[v.id]?.status.kind === 'failed'
                            ? (jobsByVideo[v.id]!.status as { message: string }).message
                            : ''}
                        >
                          Retry transcription
                        </button>
                      {/if}
                      <button class="icon" disabled={vi === 0 || busy} onclick={() => moveVideoInModule(m, v, -1)} title="Move up">↑</button>
                      <button class="icon" disabled={vi === m.videoIds.length - 1 || busy} onclick={() => moveVideoInModule(m, v, 1)} title="Move down">↓</button>
                      {#if course.modules.length > 1}
                        <select
                          disabled={busy}
                          value=""
                          onchange={(e) => {
                            const target = (e.currentTarget as HTMLSelectElement).value;
                            (e.currentTarget as HTMLSelectElement).value = '';
                            if (target) void moveVideoToOtherModule(v, target);
                          }}
                          title="Move to module"
                        >
                          <option value="">Move to…</option>
                          {#each course.modules as om (om.id)}
                            {#if om.id !== m.id}
                              <option value={om.id}>{om.title}</option>
                            {/if}
                          {/each}
                        </select>
                      {/if}
                      <button class="link" disabled={busy} onclick={() => startEdit('video', v.id, v.title)}>Rename</button>
                      <button class="link danger" disabled={busy} onclick={() => deleteVideoAt(v)}>Delete</button>
                    </div>
                  </li>

                  {#if sess}
                    <li class="recording-panel" class:awaiting={sess.state === 'awaitingDecision'}>
                      {#if sess.state === 'recording' || sess.state === 'paused'}
                        <div class="rec-status">
                          <span class="rec-dot" class:paused={sess.state === 'paused'}></span>
                          <span class="rec-timer">{fmtClock(elapsedSecondsFor(sess.id))}</span>
                          <span class="rec-label">
                            {sess.state === 'paused' ? 'Paused' : 'Recording…'}
                          </span>
                          {#if sess.sources.microphone}<span class="src-chip">Mic</span>{/if}
                          <span class="src-chip">Screen</span>
                        </div>
                        <div class="actions">
                          {#if sess.state === 'recording'}
                            <button class="ghost" disabled={busy} onclick={() => pauseFor(v.id)}>
                              Pause
                            </button>
                          {:else}
                            <button class="ghost" disabled={busy} onclick={() => resumeFor(v.id)}>
                              Resume
                            </button>
                          {/if}
                          <button class="primary" disabled={busy} onclick={() => stopFor(v.id)}>
                            Stop
                          </button>
                          <button class="link danger" disabled={busy} onclick={() => discardFor(v.id)}>
                            Discard
                          </button>
                        </div>
                      {:else if sess.state === 'awaitingDecision'}
                        <div class="rec-status">
                          <span class="rec-label">Recording finished. Keep this take?</span>
                        </div>
                        <div class="actions">
                          <button class="primary" disabled={busy} onclick={() => keepFor(v.id)}>
                            Keep
                          </button>
                          <button class="ghost" disabled={busy} onclick={() => discardFor(v.id)}>
                            Discard
                          </button>
                        </div>
                      {/if}
                    </li>
                  {/if}

                  {#if openVideoId === v.id && segs.length > 0}
                    {@const src = firstSegmentSrc(v.id)}
                    {@const t = transcriptByVideo[v.id]}
                    {@const job = jobsByVideo[v.id]}
                    <li class="video-panel">
                      <div class="player-col">
                        {#if src}
                          <!-- svelte-ignore a11y_media_has_caption -->
                          <video
                            bind:this={videoEl}
                            class="video-player"
                            src={src}
                            controls
                            preload="metadata"
                            ontimeupdate={() => onTimeUpdate(t)}
                          ></video>
                        {:else}
                          <div class="player-empty">No recorded Segment to play.</div>
                        {/if}
                      </div>
                      <div class="transcript-col">
                        {#if job && (job.status.kind === 'pending' || job.status.kind === 'running')}
                          <div class="transcript-status">
                            <span>{jobLabel(job)}</span>
                            {#if job.status.kind === 'running'}
                              <div
                                class="progress"
                                role="progressbar"
                                aria-valuenow={Math.round(job.status.fraction * 100)}
                                aria-valuemin="0"
                                aria-valuemax="100"
                              >
                                <div class="progress-fill" style="width: {Math.round(job.status.fraction * 100)}%"></div>
                              </div>
                            {/if}
                          </div>
                        {:else if job && job.status.kind === 'failed'}
                          <div class="transcript-status failed">
                            <span>Transcription failed: {job.status.message}</span>
                            <button class="link" onclick={() => retryFor(v.id)}>Retry</button>
                          </div>
                        {:else if t && t.words.length > 0}
                          <p class="transcript-words" aria-label="Transcript">
                            {#each t.words as w, i (i)}
                              <button
                                class="word"
                                class:active={activeWordIndex === i}
                                onclick={() => jumpToWord(w)}
                                title={`${w.start.toFixed(2)}s`}
                              >{w.text}</button>
                            {/each}
                          </p>
                        {:else if t}
                          <div class="transcript-status">
                            <span>Transcript is empty.</span>
                          </div>
                        {:else}
                          <div class="transcript-status">
                            <span>No transcript yet.</span>
                          </div>
                        {/if}
                      </div>
                    </li>
                  {/if}
                {/if}
              {/each}

              <li class="add-video">
                {#if newVideoModuleId === m.id}
                  <form
                    class="new-form"
                    onsubmit={(e) => {
                      e.preventDefault();
                      void commitNewVideo();
                    }}
                  >
                    <input
                      bind:this={newVideoInputEl}
                      bind:value={newVideoDraft}
                      placeholder="Video title"
                      onkeydown={(e) => {
                        if (e.key === 'Escape') cancelNewVideo();
                      }}
                    />
                    <button class="primary" type="submit" disabled={busy || !newVideoDraft.trim()}>
                      Add
                    </button>
                    <button type="button" class="ghost" onclick={cancelNewVideo} disabled={busy}>
                      Cancel
                    </button>
                  </form>
                {:else}
                  <button class="link" disabled={busy} onclick={() => openNewVideoForm(m.id)}>
                    + Add Video
                  </button>
                {/if}
              </li>
            </ul>
          </li>
        {/each}
      </ol>
    {/if}
    {:else}
      <!-- Board view: one column per workflow state, draggable Video cards. -->
      <section class="board" aria-label="Workflow board">
        {#each course.workflowStates as s (s.id)}
          {@const cards = videosInState(s.id)}
          <div
            class="board-column"
            class:drop-target={dropTargetStateId === s.id}
            role="region"
            aria-label={s.name}
            ondragover={(e) => onColumnDragOver(e, s.id)}
            ondragleave={() => onColumnDragLeave(s.id)}
            ondrop={(e) => onColumnDrop(e, s.id)}
          >
            <header class="board-column-header">
              <span class="board-column-title">{s.name}</span>
              <span class="count">{cards.length}</span>
            </header>
            <ul class="board-cards">
              {#each cards as v (v.id)}
                <li
                  class="board-card"
                  class:dragging={draggingVideoId === v.id}
                  draggable="true"
                  ondragstart={(e) => onCardDragStart(e, v.id)}
                  ondragend={onCardDragEnd}
                >
                  <div class="card-title">{v.title}</div>
                  {#if moduleTitleForVideo(v.id)}
                    <div class="card-meta">{moduleTitleForVideo(v.id)}</div>
                  {/if}
                </li>
              {/each}
              {#if cards.length === 0}
                <li class="board-empty">Drag here</li>
              {/if}
            </ul>
          </div>
        {/each}
      </section>
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
    flex-direction: column;
    gap: 0.25rem;
    margin-bottom: 1.5rem;
  }
  h1 { font-size: 1.5rem; margin: 0; }
  .folder {
    font-family: 'SF Mono', Menlo, monospace;
    font-size: 0.75rem;
    color: #666;
    background: #f0f0f0;
    padding: 1px 5px;
    border-radius: 3px;
    align-self: flex-start;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .toolbar { margin-bottom: 1rem; }
  .view-toolbar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1rem;
    gap: 0.75rem;
  }
  .view-toggle {
    display: inline-flex;
    border: 1px solid #ccc;
    border-radius: 6px;
    overflow: hidden;
    background: white;
  }
  .view-toggle button {
    background: white;
    border: none;
    padding: 0.35rem 0.8rem;
    cursor: pointer;
    font-size: 0.85rem;
    color: #444;
  }
  .view-toggle button + button { border-left: 1px solid #ddd; }
  .view-toggle button.active {
    background: #0066ff;
    color: white;
  }

  /* Workflow state badge (tree view) */
  .state-badge {
    display: inline-block;
    margin-left: 0.5rem;
    padding: 1px 7px;
    font-size: 0.7rem;
    color: #444;
    background: #eef2ff;
    border: 1px solid #d8dffb;
    border-radius: 10px;
    white-space: nowrap;
  }

  /* State editor */
  .state-editor {
    background: white;
    border: 1px solid #e0e0e0;
    border-radius: 10px;
    padding: 1rem 1.2rem;
    margin-bottom: 1rem;
  }
  .state-editor header {
    display: flex;
    flex-direction: row;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.75rem;
  }
  .state-editor h2 {
    font-size: 1rem;
    margin: 0;
  }
  ol.states {
    list-style: none;
    padding: 0;
    margin: 0 0 0.75rem 0;
  }
  li.state-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
    padding: 0.4rem 0;
    border-bottom: 1px solid #f1f1f1;
  }
  li.state-row:last-child { border-bottom: none; }

  /* Board view */
  .board {
    display: flex;
    gap: 0.75rem;
    overflow-x: auto;
    padding-bottom: 0.5rem;
  }
  .board-column {
    flex: 0 0 220px;
    background: #f4f5f7;
    border: 1px solid #e3e5e8;
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    min-height: 200px;
    transition: background-color 100ms ease, border-color 100ms ease;
  }
  .board-column.drop-target {
    background: #e6efff;
    border-color: #99baff;
  }
  .board-column-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.55rem 0.7rem;
    border-bottom: 1px solid #e3e5e8;
  }
  .board-column-title {
    font-weight: 600;
    font-size: 0.85rem;
    color: #333;
  }
  ul.board-cards {
    list-style: none;
    padding: 0.5rem;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    flex: 1;
  }
  li.board-card {
    background: white;
    border: 1px solid #d8dadd;
    border-radius: 6px;
    padding: 0.5rem 0.6rem;
    cursor: grab;
    user-select: none;
    box-shadow: 0 1px 1px rgba(0, 0, 0, 0.03);
  }
  li.board-card.dragging { opacity: 0.4; }
  .card-title { font-size: 0.88rem; color: #111; }
  .card-meta { font-size: 0.7rem; color: #777; margin-top: 2px; }
  .board-empty {
    color: #999;
    font-size: 0.75rem;
    text-align: center;
    padding: 1rem 0.5rem;
    border: 1px dashed #d0d0d0;
    border-radius: 6px;
  }
  .empty {
    background: white;
    padding: 3rem 1rem;
    border-radius: 10px;
    text-align: center;
    color: #666;
  }
  ol.modules {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }
  li.module {
    background: white;
    border-radius: 10px;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04);
    overflow: hidden;
  }
  .module-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid #eee;
    background: #fbfbfb;
  }
  .module-title {
    font-size: 1.05rem;
    font-weight: 600;
  }
  .count {
    color: #888;
    font-size: 0.8rem;
    margin-left: 0.5rem;
  }
  ul.videos {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  li.video, li.add-video {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
    padding: 0.55rem 1rem;
    border-bottom: 1px solid #f1f1f1;
  }
  li.video:last-child, li.add-video {
    border-bottom: none;
  }
  .title-area {
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 0;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
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
  .new-form {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }
  .new-form input {
    flex: 1;
    max-width: 320px;
    font-size: 1rem;
    padding: 0.45rem 0.6rem;
    border: 1px solid #ccc;
    border-radius: 4px;
  }
  button.icon {
    background: none;
    border: 1px solid #ddd;
    width: 26px;
    height: 26px;
    border-radius: 4px;
    cursor: pointer;
    color: #444;
    font-size: 0.85rem;
  }
  button.icon:disabled { opacity: 0.4; cursor: not-allowed; }
  button.link {
    background: none;
    border: none;
    color: #0066ff;
    cursor: pointer;
    padding: 0;
    font-size: 0.85rem;
  }
  button.link.danger { color: #b00020; }
  button.link:disabled { opacity: 0.5; cursor: not-allowed; }
  button.title-btn {
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: #111;
    font-size: 0.95rem;
    text-align: left;
  }
  button.title-btn:hover { text-decoration: underline; }
  select {
    font-size: 0.8rem;
    padding: 2px 4px;
    border: 1px solid #ccc;
    border-radius: 4px;
    background: white;
    cursor: pointer;
  }
  input {
    font-size: 0.95rem;
    padding: 2px 6px;
    border: 1px solid #ccc;
    border-radius: 4px;
    width: 100%;
    box-sizing: border-box;
  }
  .error {
    background: #ffe6e6;
    color: #b00020;
    padding: 0.75rem 1rem;
    border-radius: 6px;
    margin-bottom: 1rem;
    font-size: 0.85rem;
  }

  /* Orphan recovery banner */
  .orphan-banner {
    background: #fff7e6;
    border: 1px solid #f3d58b;
    padding: 0.85rem 1rem;
    border-radius: 8px;
    margin-bottom: 1rem;
  }
  .orphan-banner h2 { font-size: 0.95rem; margin: 0 0 0.25rem; color: #6c4a00; }
  .orphan-banner .hint { font-size: 0.8rem; color: #6c4a00; margin: 0 0 0.5rem; }
  .orphan-banner ul.orphan-list {
    list-style: none; padding: 0; margin: 0;
    display: flex; flex-direction: column; gap: 0.4rem;
  }
  .orphan-banner ul.orphan-list li {
    display: flex; justify-content: space-between; align-items: center;
    background: white; border: 1px solid #f1e2bb; border-radius: 6px;
    padding: 0.45rem 0.65rem; gap: 0.75rem;
  }
  .orphan-meta { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .orphan-meta code {
    font-family: 'SF Mono', Menlo, monospace; font-size: 0.7rem; color: #777;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 360px;
  }

  /* Permissions banner */
  .perms-banner {
    background: #fff0f0;
    border: 1px solid #f3b8b8;
    padding: 0.85rem 1rem;
    border-radius: 8px;
    margin-bottom: 1rem;
  }
  .perms-banner h2 { font-size: 0.95rem; margin: 0 0 0.25rem; color: #8a1a1a; }
  .perms-banner .hint { font-size: 0.8rem; color: #8a1a1a; margin: 0 0 0.5rem; }
  .perms-banner ul.perms-list {
    list-style: none; padding: 0; margin: 0 0 0.6rem 0;
    display: flex; flex-direction: column; gap: 0.4rem;
  }
  .perms-banner ul.perms-list li {
    display: grid;
    grid-template-columns: 9rem auto 1fr;
    align-items: center;
    gap: 0.5rem;
    background: white;
    border: 1px solid #f3d2d2;
    border-radius: 6px;
    padding: 0.4rem 0.6rem;
  }
  .perm-name { font-weight: 600; font-size: 0.85rem; }
  .perm-status {
    display: inline-block; padding: 1px 7px; border-radius: 10px;
    font-size: 0.7rem; text-transform: uppercase;
    background: #f4f4f4; color: #555;
  }
  .perm-status.status-denied { background: #ffe5e5; color: #8a1a1a; }
  .perm-status.status-notDetermined { background: #fff7e6; color: #6c4a00; }
  .perm-status.status-restricted { background: #eee; color: #555; }

  /* Per-Video recording bits */
  .segments-badge {
    display: inline-block;
    margin-left: 0.4rem;
    padding: 1px 7px;
    font-size: 0.7rem;
    color: #2c5b1a;
    background: #e6f6df;
    border: 1px solid #c4e3b6;
    border-radius: 10px;
  }
  .rec-badge {
    display: inline-flex; align-items: center; gap: 4px;
    margin-left: 0.4rem;
    padding: 1px 7px;
    font-size: 0.7rem;
    font-weight: 600;
    color: #b00020;
    background: #ffe5e5;
    border: 1px solid #f3b8b8;
    border-radius: 10px;
    animation: pulse 1.4s ease-in-out infinite;
  }
  .rec-badge.paused {
    color: #6c4a00; background: #fff7e6; border-color: #f3d58b;
    animation: none;
  }
  button.rec-btn {
    color: #b00020;
    font-weight: 600;
  }
  button.rec-btn:disabled { color: #888; }

  /* Recording panel under a video row */
  li.recording-panel {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
    padding: 0.6rem 1rem;
    background: #fff7f7;
    border-top: 1px solid #f3d2d2;
    border-bottom: 1px solid #f3d2d2;
  }
  li.recording-panel.awaiting {
    background: #f3f7ff; border-color: #c8d9f5;
  }
  .rec-status {
    display: flex; align-items: center; gap: 0.5rem; flex: 1;
  }
  .rec-dot {
    width: 10px; height: 10px; border-radius: 50%;
    background: #d62a2a;
    box-shadow: 0 0 0 0 rgba(214, 42, 42, 0.6);
    animation: pulse 1.4s ease-in-out infinite;
  }
  .rec-dot.paused { background: #c79a2c; animation: none; }
  .rec-timer {
    font-family: 'SF Mono', Menlo, monospace;
    font-size: 0.95rem;
    color: #333;
    min-width: 56px;
  }
  .rec-label { font-size: 0.85rem; color: #555; }
  .src-chip {
    display: inline-block; padding: 1px 7px; font-size: 0.7rem;
    color: #555; background: #eef0f3; border: 1px solid #d8dadd;
    border-radius: 10px;
  }
  @keyframes pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50%      { opacity: 0.55; transform: scale(0.85); }
  }

  /* Transcription badge */
  .trx-badge {
    display: inline-block;
    margin-left: 0.4rem;
    padding: 1px 7px;
    font-size: 0.7rem;
    border-radius: 10px;
    background: #eef0f3;
    border: 1px solid #d8dadd;
    color: #555;
    white-space: nowrap;
  }
  .trx-badge.running { background: #fff6d9; border-color: #f3d58b; color: #6c4a00; }
  .trx-badge.failed { background: #ffe5e5; border-color: #f3b8b8; color: #8a1a1a; }
  .trx-badge.done { background: #e6f6df; border-color: #c4e3b6; color: #2c5b1a; }

  /* Video panel: player + transcript side by side */
  li.video-panel {
    display: grid;
    grid-template-columns: minmax(0, 1.2fr) minmax(0, 1fr);
    gap: 1rem;
    padding: 0.75rem 1rem 1rem 1rem;
    background: #f8fafd;
    border-top: 1px solid #e3e5e8;
    border-bottom: 1px solid #e3e5e8;
  }
  .player-col { min-width: 0; }
  .transcript-col { min-width: 0; max-height: 320px; overflow-y: auto; }
  .video-player {
    width: 100%;
    max-height: 320px;
    background: black;
    border-radius: 6px;
  }
  .player-empty {
    background: #eee; padding: 2rem; text-align: center; border-radius: 6px; color: #777;
  }
  .transcript-status {
    background: white;
    border: 1px solid #d8dadd;
    border-radius: 6px;
    padding: 0.6rem 0.75rem;
    font-size: 0.85rem;
    color: #555;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .transcript-status.failed { background: #fff0f0; border-color: #f3b8b8; color: #8a1a1a; }
  .progress {
    width: 100%; height: 6px; background: #eee; border-radius: 3px; overflow: hidden;
  }
  .progress-fill {
    height: 100%; background: #0066ff; transition: width 120ms linear;
  }
  .transcript-words {
    margin: 0;
    background: white;
    border: 1px solid #d8dadd;
    border-radius: 6px;
    padding: 0.6rem 0.75rem;
    line-height: 1.6;
    color: #333;
    font-size: 0.92rem;
  }
  .word {
    background: none;
    border: none;
    padding: 1px 2px;
    margin: 0;
    color: inherit;
    font: inherit;
    cursor: pointer;
    border-radius: 3px;
  }
  .word:hover { background: #eef2ff; }
  .word.active {
    background: #0066ff;
    color: white;
  }
</style>
