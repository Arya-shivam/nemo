# Nemo launch film review

## Creative calls

The chosen storyline is **Find it, then file it**. The cut opens with lost screenshots and a messy Downloads folder, shows text or visual-description image search, then folder categorization by names or optional image previews. Review, confirmation, undo, and the Nemo end card close the story. The user-supplied Nemo fish mark and the actual repository CTA are on the end card: `github.com/Arya-shivam/nemo`.

The alternatives **Names first** and **Nothing moves without you** were not built. The film has no voiceover. It reuses the existing 119.9 BPM music and bundled chime and impact. The search header comes from Nemo with a non-personal test query. Categorization frames and example filenames are synthetic fixtures; no personal screenshots or Downloads files appear.

## Checks

`npm run check`, `npx hyperframes lint`, and `npx hyperframes check --json` passed with 0 errors and 0 warnings. The delivered MP4 is 18 seconds, 1920 × 1080, 30 fps, H.264 video, and AAC stereo audio at 48 kHz. `ffprobe` confirmed the output duration and streams.

Reviewed all 12 beat frames, five seam sheets, the 360 px phone sheet, hook and end-card motion samples, and decoded render frames around the proof entrance and CTA. Opened the HyperFrames desktop app preview at `http://localhost:3002/#project/launch-video` and verified the project timeline and preview loaded.

Audio analysis measured -20.1 LUFS integrated, -17.3 LUFS maximum short-term, -1.6 dBTP true peak, zero clipped samples, and no warnings. The sound was measured, not auditioned.

## Remaining weakness

The synthetic filename-to-category proof row is small at 1080p. With another pass, enlarge that actual UI detail or hold it longer without obscuring the names-only message.

## Warnings kept

HyperFrames reported no warnings. The folder-to-proof handoff starts one frame before 7 seconds so the visible mode controls enter on the first proof frame instead of a blank frame.
