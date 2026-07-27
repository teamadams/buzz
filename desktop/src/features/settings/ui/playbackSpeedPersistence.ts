type MutableValue<T> = {
  current: T;
};

export type PlaybackSpeedPersistenceState = {
  confirmedSpeed: MutableValue<number>;
  desiredSpeed: MutableValue<number>;
  flushPromise: MutableValue<Promise<void> | null>;
};

type PlaybackSpeedPersistenceCallbacks = {
  persist: (speed: number) => Promise<void>;
  setSpeed: (speed: number) => void;
  setError: (error: string | null) => void;
};

export function commitPlaybackSpeed(
  state: PlaybackSpeedPersistenceState,
  callbacks: PlaybackSpeedPersistenceCallbacks,
  nextSpeed: number,
) {
  state.desiredSpeed.current = nextSpeed;
  callbacks.setSpeed(nextSpeed);
  callbacks.setError(null);
  ensurePlaybackSpeedFlush(state, callbacks);
}

function ensurePlaybackSpeedFlush(
  state: PlaybackSpeedPersistenceState,
  callbacks: PlaybackSpeedPersistenceCallbacks,
) {
  if (state.flushPromise.current) return;

  const flush = flushPlaybackSpeed(state, callbacks).finally(() => {
    if (state.flushPromise.current !== flush) return;
    state.flushPromise.current = null;
    if (state.desiredSpeed.current !== state.confirmedSpeed.current) {
      ensurePlaybackSpeedFlush(state, callbacks);
    }
  });
  state.flushPromise.current = flush;
}

async function flushPlaybackSpeed(
  state: PlaybackSpeedPersistenceState,
  callbacks: PlaybackSpeedPersistenceCallbacks,
) {
  while (state.desiredSpeed.current !== state.confirmedSpeed.current) {
    const target = state.desiredSpeed.current;
    try {
      await callbacks.persist(target);
      state.confirmedSpeed.current = target;
    } catch (cause) {
      if (state.desiredSpeed.current === target) {
        state.desiredSpeed.current = state.confirmedSpeed.current;
        callbacks.setSpeed(state.confirmedSpeed.current);
        callbacks.setError(String(cause));
        return;
      }
    }
  }
}
