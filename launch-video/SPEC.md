# Nemo launch film specification

## Output

- 1920 × 1080, 30 fps, 18 seconds, 16:9.
- Six beats: problem, image search, folder-sort reveal, names/image-mode proof, user approval, undo and brand close.
- Music-led; no voiceover. Reuse the existing measured 119.9 BPM synth bed and bundled confirmation/impact effects.
- HyperFrames render-time motion blur on the root, shutter 120. Verify in the final check.

## Audience and claim

Someone who loses images in screenshot collections or has an unstructured Downloads folder. Nemo searches indexed images using text and visual descriptions. Its separate folder feature suggests categories from filenames or optional image previews, then asks the user to review and confirm. The UI shown is actual app UI; no claim about API speed, automatic execution, or cloud behavior beyond the displayed names-only mode.

## Shot frames

- 0–60: “LOST SCREENSHOTS. / MESSY DOWNLOADS.” Filename fragments move across a dark field. Typography, not a fake desktop.
- 60–150: Real Nemo search header and the descriptive test query, with “SEARCH BY WORDS. / Describe what you saw.” No result or user image invented.
- 150–210: Real folder-categorization screen and “SORT A FOLDER. / By names or image previews.”
- 210–330: Actual mode controls, then the synthetic review row “Invoice_October_2026.pdf” to “Finance and receipts.” Explanatory connector is a graphic overlay, not a control.
- 330–405: Actual confirmation screen with “REVIEW. THEN CONFIRM.” No implied click or success state.
- 405–450: Actual undo detail and “UNDO BUILT IN.”
- 450–540: User-supplied Nemo fish image, product name, tagline, and repository CTA. End card holds through frame 539.

## Look and movement

- Inter with 900/700 weights. Flat paper, ink, and vermilion palette. No gradients, icon cards, or invented product screens.
- The cut grid is 0.5 s, with the folder-to-mode handoff one frame early so visible controls are already on the first proof frame. Hook labels whip in; real screens settle with short pushes; labels move in opposite directions; final mark carries into the end card. Entrances vary by role. Titles settle before the next action.
- Asset provenance: supplied `Nemo-FN.webp` copied to `assets/nemo-logo.webp`; `nemo-search-header.png` captured from the running Nemo app; all `nemo-category-*.png` crops from the project's synthetic categorization test captures. No user screenshots or Downloads files.

## Sound

The existing `assets/bgm/track.wav` measures 119.9 BPM and runs the full 18 seconds, with a 1 second end fade. Reuse `assets/sfx/chime.mp3` at approval and `assets/sfx/impact-bass-1.mp3` on the logo arrival. No voice track. The score is not newly generated.
