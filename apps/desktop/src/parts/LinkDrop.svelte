<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../core/store.svelte";
  import { carriesFiles, carriesLink, isTextTarget, linkText } from "../core/incoming";

  /**
   * Links that arrive without typing: Ctrl+V anywhere outside a text field, and a link dragged in
   * from a browser's address bar, Teams or Outlook. The native file drop is off for this window
   * (`dragDropEnabled: false`), so these are plain HTML5 events; only text and URLs are taken,
   * files are refused. The clipboard is read only from the paste event itself, never in the
   * background.
   *
   * A browser keeps the dragged data hidden until the drop, so the overlay cannot name the host
   * yet; it says what to expect, and New download shows the host once the link is opened.
   */
  let dragging = $state(false);
  let lastOver = 0;
  let watchdog: ReturnType<typeof setInterval> | undefined;

  function hide() {
    dragging = false;
    clearInterval(watchdog);
    watchdog = undefined;
  }

  function onPaste(e: ClipboardEvent) {
    if (isTextTarget(e.target as HTMLElement | null) || isTextTarget(document.activeElement as HTMLElement | null)) return;
    const text = e.clipboardData?.getData("text/plain") ?? "";
    // Pasting an image or nothing at all is not ours to answer.
    if (text.trim() === "") return;
    e.preventDefault();
    void app.receiveLink(text);
  }

  function onDragOver(e: DragEvent) {
    const types = Array.from(e.dataTransfer?.types ?? []);
    // Text dragged into a text field is the field's business.
    if (isTextTarget(e.target as HTMLElement | null) && !carriesFiles(types)) {
      hide();
      return;
    }
    // Cancel the event for everything else: otherwise the webview would open a dropped file as a page.
    e.preventDefault();
    if (!e.dataTransfer) return;
    if (carriesFiles(types) || !carriesLink(types) || app.tourOpen || app.closePrompt) {
      e.dataTransfer.dropEffect = "none";
      return;
    }
    e.dataTransfer.dropEffect = "copy";
    lastOver = Date.now();
    if (!dragging) {
      dragging = true;
      // dragover repeats while the pointer is over the window; when it stops, the drag has left.
      watchdog = setInterval(() => {
        if (Date.now() - lastOver > 700) hide();
      }, 250);
    }
  }

  /** Leaving the window shows up as a dragleave at its edge; moving between elements does not. */
  function onDragLeave(e: DragEvent) {
    const out = e.clientX <= 0 || e.clientY <= 0 || e.clientX >= innerWidth || e.clientY >= innerHeight;
    if (e.relatedTarget === null && out) hide();
  }

  function onDrop(e: DragEvent) {
    const dt = e.dataTransfer;
    const types = Array.from(dt?.types ?? []);
    if (isTextTarget(e.target as HTMLElement | null) && !carriesFiles(types)) return;
    e.preventDefault();
    hide();
    if (!dt) return;
    if (!carriesLink(types) || carriesFiles(types)) {
      if (carriesFiles(types)) app.setNotice("AmpSync downloads from a SharePoint link. Drop the link, not a file.");
      return;
    }
    void app.receiveLink(linkText(dt.getData("text/uri-list"), dt.getData("text/plain")));
  }

  onMount(() => () => hide());
</script>

<svelte:window onpaste={onPaste} ondragover={onDragOver} ondragleave={onDragLeave} ondrop={onDrop} ondragend={hide} />

{#if dragging}
  <div class="veil" aria-hidden="true">
    <div class="frame"></div>
    <div class="card">
      <svg viewBox="0 0 34 34" class="glyph">
        <path d="M9 7v10M14 7v12M19 7v10M24 7v12" stroke="var(--signal)" stroke-width="2.4" stroke-linecap="round" />
        <path d="M17 20v5M13.5 22.5L17 26l3.5-3.5" stroke="var(--fg)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none" />
        <path d="M8 29h18" stroke="var(--fg)" stroke-width="2.4" stroke-linecap="round" />
      </svg>
      <h2>Drop to download</h2>
      <p>A SharePoint folder link, dragged from a browser address bar, Teams or Outlook.</p>
      <span class="tag">link only · files are ignored</span>
    </div>
  </div>
{/if}

<style>
  .veil {
    position: fixed; inset: 0; z-index: 80; pointer-events: none; display: grid; place-items: center;
    background: rgba(6, 8, 11, .74); animation: veil-in .12s ease-out;
  }
  .frame { position: absolute; inset: 14px; border: 2px dashed var(--signal); border-radius: 6px; opacity: .85; }
  .card {
    position: relative; display: grid; justify-items: center; gap: 6px; width: min(420px, calc(100vw - 64px)); padding: 22px 24px 20px;
    background: var(--raise); border: 1px solid var(--line); border-top: 2px solid var(--signal); border-radius: 4px;
    box-shadow: 0 12px 32px rgba(0, 0, 0, .4); text-align: center;
  }
  .glyph { width: 44px; height: 44px; margin-bottom: 4px; }
  h2 { margin: 0; font: 600 20px/1.2 var(--f-display); letter-spacing: .01em; }
  p { margin: 0; font-size: 13px; color: var(--muted); text-wrap: balance; }
  .tag { margin-top: 8px; font: 11px var(--f-mono); color: var(--muted); letter-spacing: .04em; }
  @keyframes veil-in { from { opacity: 0; } to { opacity: 1; } }
  @media (prefers-reduced-motion: reduce) {
    .veil { animation: none; }
  }
</style>
