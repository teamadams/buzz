import assert from "node:assert/strict";
import test from "node:test";

import {
  createInitialMembershipGate,
  createLatestStateGate,
  createOrderedSpeaker,
  speakableAgentText,
} from "./ttsLiveMessages.ts";

const agents = new Set(["agent"]);
const base = {
  id: "1",
  kind: 9,
  pubkey: "agent",
  content: "Hello there",
  tags: [],
};

test("speaks only new agent-authored text message events", () => {
  assert.equal(speakableAgentText(base, agents, "human"), "Hello there");
  assert.equal(
    speakableAgentText({ ...base, kind: 7 }, agents, "human"),
    null,
    "reactions and other event kinds are excluded",
  );
  assert.equal(
    speakableAgentText({ ...base, kind: 10 }, agents, "human"),
    null,
    "edits and status events are excluded",
  );
  assert.equal(
    speakableAgentText({ ...base, pubkey: "human" }, agents, "human"),
    null,
    "human-authored messages are excluded",
  );
  assert.equal(
    speakableAgentText({ ...base, content: " " }, agents, "human"),
    null,
    "empty and non-text content are excluded",
  );
  assert.equal(
    speakableAgentText(
      { ...base, content: "[System] tool started" },
      agents,
      "human",
    ),
    null,
    "legacy system rows are excluded",
  );
});

test("strips attachment markup and skips attachment-only events", () => {
  const url = "https://cdn.example/voice.png";
  const tags = [["imeta", `url ${url}`, "m image/png"]];
  assert.equal(
    speakableAgentText(
      { ...base, content: `![image](${url})`, tags },
      agents,
      "human",
    ),
    null,
  );
  assert.equal(
    speakableAgentText(
      { ...base, content: `Here is the diagram.\n\n![image](${url})`, tags },
      agents,
      "human",
    ),
    "Here is the diagram.",
  );
  assert.equal(
    speakableAgentText(
      { ...base, content: `||\n![image](${url})\n||`, tags },
      agents,
      "human",
    ),
    null,
  );
});

test("queues agent messages in live thread arrival order", async () => {
  const spoken = [];
  let releaseFirst;
  const firstBlocked = new Promise((resolve) => {
    releaseFirst = resolve;
  });
  const speaker = createOrderedSpeaker(async (text) => {
    if (text === "first") await firstBlocked;
    spoken.push(text);
  }, assert.fail);

  speaker.enqueue("first");
  speaker.enqueue("second");
  await Promise.resolve();
  assert.deepEqual(spoken, []);
  releaseFirst();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(spoken, ["first", "second"]);
});

test("disabling cancels queued speech and rejects new messages until enabled", async () => {
  const invoked = [];
  let releaseFirst;
  const firstBlocked = new Promise((resolve) => {
    releaseFirst = resolve;
  });
  const speaker = createOrderedSpeaker(async (text) => {
    invoked.push(text);
    if (text === "first") await firstBlocked;
  }, assert.fail);

  speaker.enqueue("first");
  speaker.enqueue("queued-before-off");
  await Promise.resolve();
  speaker.setEnabled(false);
  speaker.enqueue("while-off");
  releaseFirst();
  await new Promise((resolve) => setTimeout(resolve, 0));
  speaker.setEnabled(true);
  speaker.enqueue("after-on");
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(invoked, ["first", "after-on"]);
});

test("a live TTS state event supersedes a delayed bootstrap result", () => {
  const applied = [];
  const gate = createLatestStateGate((enabled) => applied.push(enabled));
  const applyBootstrap = gate.beginSnapshot();

  gate.applyEvent(false);
  applyBootstrap(true);

  assert.deepEqual(applied, [false]);
});

test("buffers initial live events until membership resolves in order", () => {
  const delivered = [];
  const gate = createInitialMembershipGate((event) => delivered.push(event));
  gate.push("first");
  gate.push("second");
  assert.deepEqual(delivered, []);
  gate.succeed();
  gate.push("third");
  assert.deepEqual(delivered, ["first", "second", "third"]);
});

test("drops the initial buffer fail-closed when membership lookup fails", () => {
  const delivered = [];
  const gate = createInitialMembershipGate((event) => delivered.push(event));
  gate.push("unverified");
  gate.fail();
  assert.deepEqual(delivered, []);
});
