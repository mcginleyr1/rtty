# Images

How programs show images in rtty with kitty's graphics protocol: where an image is drawn, how it
moves with the text, and the cell size rtty reports for clients to size images by.

## Drawing

Programs show images with kitty's graphics protocol: rtty-vt reads the commands (the subset is in
its [README](../vt/README.md)) and keeps the images and where they are placed, and each frame draws
them with GPUI's images, clipped to the pane: those with a z under −2³⁰ beneath the cells'
backgrounds, other negative ones beneath the text, the rest over it.

- An image is anchored to the cell its top left corner is on, so it scrolls with the text into the
  scrollback and back, and moves with that cell when a resize reflows the screen.
- It keeps its size in cells, as the cell size was when it was placed (`line_height` included): a
  later font change scales it, and a later `line_height` change stretches it with its rows.
- `clear` (ED 2), the alternate screen's clearing and a reset take the images on the screen;
  erasing text leaves them.
- Unicode placeholders work too: cells of U+10EEEE draw no glyph but their part of the image, so
  images survive tmux and tutti, which pass the placeholder text along.
- Each image is decoded once, by rtty-vt, held as one GPUI image until rtty-vt evicts it (256 MiB
  of images per pane, the oldest first), and its pixels go to the GPU once.

`graphics = false` drops every graphics command, as a terminal without the protocol does.

## The cell size

Clients size images by the cell size in pixels, which rtty reports in the display's device
pixels, as Ghostty and WezTerm do: the pty's `ws_xpixel` and `ws_ypixel`, `CSI 14 t` and `16 t`
(on a Retina display a cell of 8×17 points is 16×34), and SGR-pixel mouse reports (mode 1016),
which count a pointer's offset in those same pixels. So an image shown at its own size covers its
pixels one to one. The scale is the window's, as each layout finds it.
