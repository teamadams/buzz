export type LiveTtsEvent = {
  id: string;
  kind: number;
  pubkey: string;
  content: string;
  tags: string[][];
};

function textWithoutAttachments(event: LiveTtsEvent): string {
  const urls = new Set(
    event.tags
      .filter((tag) => tag[0] === "imeta")
      .flatMap((tag) =>
        tag
          .slice(1)
          .filter((field) => field.startsWith("url "))
          .map((field) => field.slice(4)),
      ),
  );
  if (urls.size === 0) return event.content;
  const withoutMedia = event.content
    .split("\n")
    .filter(
      (line) => !Array.from(urls).some((url) => line.includes(`](${url})`)),
    )
    .join("\n");
  return withoutMedia.replace(
    /(^|\n)\s*\|\|\s*\n(?:\s*\n)*\s*\|\|\s*(?=\n|$)/gu,
    "$1",
  );
}

export function speakableAgentText(
  event: LiveTtsEvent,
  agentPubkeys: ReadonlySet<string>,
  selfPubkey: string | null,
): string | null {
  if (event.kind !== 9) return null;
  if (!agentPubkeys.has(event.pubkey)) return null;
  if (event.pubkey === selfPubkey) return null;
  const content = textWithoutAttachments(event).trim();
  if (content.length <= 1) return null;
  if (content.startsWith("[System]")) return null;
  return content;
}

/**
 * Serialize native speak calls so live messages enter the bounded Pocket queue
 * in thread arrival order even when the bridge resolves calls asynchronously.
 */
export function createOrderedSpeaker(
  speak: (text: string) => Promise<void>,
  onError: (error: unknown) => void,
): {
  enqueue: (text: string) => void;
  setEnabled: (enabled: boolean) => void;
} {
  let tail = Promise.resolve();
  let enabled = true;
  let generation = 0;
  return {
    enqueue(text) {
      if (!enabled) return;
      const queuedGeneration = generation;
      tail = tail
        .then(() => {
          if (!enabled || generation !== queuedGeneration) return;
          return speak(text);
        })
        .catch(onError);
    },
    setEnabled(nextEnabled) {
      if (!nextEnabled) generation += 1;
      enabled = nextEnabled;
    },
  };
}

/** Ensure a delayed bootstrap snapshot cannot overwrite a newer live event. */
export function createLatestStateGate<T>(apply: (value: T) => void): {
  applyEvent: (value: T) => void;
  beginSnapshot: () => (value: T) => void;
} {
  let revision = 0;
  return {
    applyEvent(value) {
      revision += 1;
      apply(value);
    },
    beginSnapshot() {
      const snapshotRevision = revision;
      return (value) => {
        if (revision === snapshotRevision) apply(value);
      };
    },
  };
}

/** Hold live events until the first authoritative agent-membership lookup. */
export function createInitialMembershipGate<T>(deliver: (event: T) => void): {
  push: (event: T) => void;
  succeed: () => void;
  fail: () => void;
} {
  let settled = false;
  let pending: T[] = [];
  return {
    push(event) {
      if (settled) deliver(event);
      else pending.push(event);
    },
    succeed() {
      if (settled) return;
      settled = true;
      const buffered = pending;
      pending = [];
      for (const event of buffered) deliver(event);
    },
    fail() {
      settled = true;
      pending = [];
    },
  };
}
