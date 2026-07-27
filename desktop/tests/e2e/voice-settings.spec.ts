import { expect, test } from "@playwright/test";

import { waitForAnimations } from "../helpers/animations";
import { installMockBridge } from "../helpers/bridge";
import { openSettings } from "../helpers/settings";

const SCREENSHOT_PATH = "test-results/voice-settings/pocket-voices.png";

test.describe("Pocket voice settings", () => {
  test.use({ viewport: { width: 1100, height: 760 } });

  test("selects and retains a bundled voice while text to speech is off", async ({
    page,
  }) => {
    await installMockBridge(page);
    await page.goto("/", { waitUntil: "domcontentloaded" });
    await openSettings(page, "voice");

    const card = page.getByTestId("settings-voice");
    await expect(card).toBeVisible();
    await expect(
      page.getByText("Agent text to speech", { exact: true }),
    ).toBeVisible();

    await page.getByTestId("pocket-voice-selector").click();
    await page.getByRole("menuitemradio", { name: "Marius" }).click();
    await expect(page.getByTestId("pocket-voice-selector")).toContainText(
      "Marius",
    );

    await page.getByTestId("agent-text-to-speech-toggle").click();
    await expect(page.getByTestId("pocket-voice-controls")).toHaveAttribute(
      "aria-disabled",
      "true",
    );
    await expect(page.getByTestId("pocket-voice-selector")).toContainText(
      "Marius",
    );

    const savedCommands = await page.evaluate(() =>
      (window.__BUZZ_E2E_COMMAND_LOG__ ?? [])
        .filter((entry) =>
          ["set_pocket_voice", "set_tts_enabled"].includes(entry.command),
        )
        .map((entry) => ({ command: entry.command, payload: entry.payload })),
    );
    expect(savedCommands).toEqual([
      {
        command: "set_pocket_voice",
        payload: { voiceKey: "pocket:marius" },
      },
      {
        command: "set_tts_enabled",
        payload: { enabled: false },
      },
    ]);
  });

  test("captures the complete two-voice settings surface", async ({ page }) => {
    await installMockBridge(page, {
      ttsSettings: {
        version: 1,
        agentTextToSpeech: true,
        voicePreferences: ["pocket:marius"],
      },
    });
    await page.goto("/", { waitUntil: "domcontentloaded" });
    await openSettings(page, "voice");

    const card = page.getByTestId("settings-voice");
    await expect(card).toBeVisible();
    await expect(page.getByTestId("pocket-voice-selector")).toContainText(
      "Marius",
    );
    await waitForAnimations(page);
    await card.screenshot({ path: SCREENSHOT_PATH });
  });
});
