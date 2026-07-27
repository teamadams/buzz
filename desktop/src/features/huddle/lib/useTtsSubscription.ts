import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import * as React from "react";

import { buildHuddleTtsLiveFilter } from "@/shared/api/relayChannelFilters";
import { relayClient } from "@/shared/api/relayClient";
import {
  createInitialMembershipGate,
  createLatestStateGate,
  createOrderedSpeaker,
  speakableAgentText,
} from "./ttsLiveMessages";

const AGENT_PUBKEY_REFRESH_INTERVAL_MS = 30_000;

/**
 * Subscribe to agent TTS messages on the ephemeral huddle channel.
 * Pipes agent kind:9 messages to `speak_agent_message` on the Rust backend.
 *
 * Extracted from HuddleContext to keep file sizes manageable.
 */
export function useTtsSubscription(
  ephemeralChannelId: string | null,
  selfPubkeyRef: React.RefObject<string | null>,
) {
  React.useEffect(() => {
    if (!ephemeralChannelId) return;

    let disposed = false;
    let cleanup: (() => void) | null = null;
    let unlistenHuddleState: (() => void) | null = null;

    // ── Agent identity (authoritative, fail-closed) ───────────────────────
    //
    // Fetch the ephemeral channel's member list from the relay REST API and
    // identify agents by their "bot" role. This is authoritative — it works
    // for both creators and joiners, and reflects mid-huddle agent additions.
    //
    // FAIL-CLOSED: agentsLoaded starts false. Until the fetch succeeds and
    // populates agentPubkeys, NO messages are spoken. An empty set after a
    // successful fetch means "no agents in the huddle" → still mute.
    let agentsLoaded = false;
    const agentPubkeys = new Set<string>();

    const speakInOrder = createOrderedSpeaker(
      async (text) => {
        if (!disposed) {
          await invoke("speak_agent_message", { text });
        }
      },
      (err) => {
        console.warn("[huddle] TTS speak failed:", err);
      },
    );

    const deliver = (event: Parameters<typeof speakableAgentText>[0]) => {
      if (!agentsLoaded || disposed) return;
      const text = speakableAgentText(
        event,
        agentPubkeys,
        selfPubkeyRef.current,
      );
      if (text) speakInOrder.enqueue(text);
    };
    const initialMembershipGate = createInitialMembershipGate(deliver);

    async function loadAgentPubkeys(initial = false) {
      try {
        const pubkeys = await invoke<string[]>("get_huddle_agent_pubkeys");
        if (disposed) return;
        agentPubkeys.clear();
        for (const pk of pubkeys) agentPubkeys.add(pk);
        agentsLoaded = true;
        if (initial) {
          initialMembershipGate.succeed();
        }
      } catch (e) {
        // Fail-closed on ALL failures, including refresh after prior success.
        // Clear the set and mark as not loaded — TTS goes mute until the
        // next successful refresh. Stale membership must never authorize speech.
        agentPubkeys.clear();
        agentsLoaded = false;
        if (initial) {
          initialMembershipGate.fail();
        }
        console.error("[huddle] Failed to load agent pubkeys:", e);
      }
    }

    // Initial load + periodic refresh (catches mid-huddle agent additions).
    void loadAgentPubkeys(true);
    const agentRefreshId = window.setInterval(() => {
      void loadAgentPubkeys();
    }, AGENT_PUBKEY_REFRESH_INTERVAL_MS);

    // Install the state listener before requesting a snapshot. If a newer
    // event arrives while IPC is pending, it supersedes the stale snapshot.
    const ttsStateGate = createLatestStateGate<{ tts_enabled: boolean }>(
      (state) => {
        if (!disposed) speakInOrder.setEnabled(state.tts_enabled);
      },
    );
    void listen<{ tts_enabled: boolean }>("huddle-state-changed", (event) => {
      if (!disposed) ttsStateGate.applyEvent(event.payload);
    })
      .then((unlisten) => {
        if (disposed) {
          unlisten();
          return;
        }
        unlistenHuddleState = unlisten;
        const applyBootstrap = ttsStateGate.beginSnapshot();
        void invoke<{ tts_enabled: boolean }>("get_huddle_state")
          .then((state) => {
            if (!disposed) applyBootstrap(state);
          })
          .catch((err) => {
            if (!disposed) applyBootstrap({ tts_enabled: false });
            console.warn("[huddle] Failed to load TTS state:", err);
          });
      })
      .catch((err) => {
        speakInOrder.setEnabled(false);
        console.warn("[huddle] Failed to listen for TTS state:", err);
      });

    // ── Live-only subscription ───────────────────────────────────────────
    // A kind:9, limit:0 subscription receives future fan-out while the relay
    // returns no stored rows, including pre-join rows from the current second.
    // Event-ID dedup handles reconnect replay (same event arriving twice).
    const seenEventIds = new Set<string>();
    const seenOrder: string[] = [];
    const MAX_SEEN_EVENTS = 5000;
    relayClient
      .subscribeLive(buildHuddleTtsLiveFilter(ephemeralChannelId), (event) => {
        if (disposed) return;
        // Dedup by event ID (covers reconnect replay).
        if (seenEventIds.has(event.id)) return;
        seenEventIds.add(event.id);
        seenOrder.push(event.id);
        if (seenOrder.length > MAX_SEEN_EVENTS) {
          const oldest = seenOrder.shift();
          if (oldest !== undefined) seenEventIds.delete(oldest);
        }

        // Preserve arrival order while the initial authoritative membership
        // lookup is pending. A failed lookup clears this buffer fail-closed.
        initialMembershipGate.push(event);
      })
      .then((dispose) => {
        if (disposed) {
          void dispose();
          return;
        }
        cleanup = () => void dispose();
      })
      .catch((err) => {
        console.error("[huddle] TTS subscription failed:", err);
      });

    return () => {
      disposed = true;
      speakInOrder.setEnabled(false);
      cleanup?.();
      unlistenHuddleState?.();
      window.clearInterval(agentRefreshId);
    };
  }, [ephemeralChannelId, selfPubkeyRef]);
}
