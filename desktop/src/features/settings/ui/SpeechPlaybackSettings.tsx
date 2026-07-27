import { useEffect, useRef, useState } from "react";
import { getTtsPlaybackSpeed, setTtsPlaybackSpeed } from "@/shared/api/tauri";
import { Button } from "@/shared/ui/button";
import { commitPlaybackSpeed } from "./playbackSpeedPersistence";
import { SettingsOptionGroup, SettingsOptionRow } from "./SettingsOptionGroup";

const DEFAULT_SPEED = 1;
const MIN_SPEED = 0.75;
const MAX_SPEED = 1.5;
const SPEED_STEP = 0.05;

export function SpeechPlaybackSettings() {
  const [speed, setSpeed] = useState(DEFAULT_SPEED);
  const [error, setError] = useState<string | null>(null);
  const confirmedSpeed = useRef(DEFAULT_SPEED);
  const desiredSpeed = useRef(DEFAULT_SPEED);
  const flushPromise = useRef<Promise<void> | null>(null);

  useEffect(() => {
    let active = true;
    getTtsPlaybackSpeed()
      .then((savedSpeed) => {
        if (active) {
          confirmedSpeed.current = savedSpeed;
          desiredSpeed.current = savedSpeed;
          setSpeed(savedSpeed);
        }
      })
      .catch((cause) => {
        if (active) setError(String(cause));
      });
    return () => {
      active = false;
    };
  }, []);

  const commitSpeed = (nextSpeed: number) => {
    commitPlaybackSpeed(
      { confirmedSpeed, desiredSpeed, flushPromise },
      { persist: setTtsPlaybackSpeed, setError, setSpeed },
      nextSpeed,
    );
  };

  return (
    <SettingsOptionGroup>
      <SettingsOptionRow className="items-start">
        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between gap-4">
            <div>
              <label
                className="text-sm font-medium"
                htmlFor="speech-playback-speed"
              >
                Speech playback speed
              </label>
              <p className="text-sm font-normal text-muted-foreground">
                Changes generated speech playback without changing voice pitch.
              </p>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <span
                aria-live="polite"
                className="min-w-10 text-right text-sm tabular-nums"
              >
                {speed.toFixed(2)}x
              </span>
              <Button
                disabled={speed === DEFAULT_SPEED}
                onClick={() => commitSpeed(DEFAULT_SPEED)}
                size="sm"
                type="button"
                variant="outline"
              >
                Reset
              </Button>
            </div>
          </div>
          <input
            aria-valuetext={`${speed.toFixed(2)} times`}
            className="mt-3 w-full accent-primary"
            id="speech-playback-speed"
            max={MAX_SPEED}
            min={MIN_SPEED}
            onBlur={(event) => commitSpeed(Number(event.currentTarget.value))}
            onChange={(event) => setSpeed(Number(event.currentTarget.value))}
            onKeyUp={(event) => commitSpeed(Number(event.currentTarget.value))}
            onPointerUp={(event) =>
              commitSpeed(Number(event.currentTarget.value))
            }
            step={SPEED_STEP}
            type="range"
            value={speed}
          />
          {error ? (
            <p className="mt-1 text-xs text-destructive" role="alert">
              Could not save playback speed: {error}
            </p>
          ) : null}
        </div>
      </SettingsOptionRow>
    </SettingsOptionGroup>
  );
}
