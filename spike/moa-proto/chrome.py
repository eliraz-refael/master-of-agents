"""THROWAWAY PROTOTYPE: the shared look of moa's panels. One palette (Nord, lightened for
contrast; the same hex colours as tmux.conf, exact where the terminal has true colour (`wrapper`),
else mapped to the 256-colour cube) and a rounded frame with
the title on its top edge. tmux can't round its own pane borders, so moa's panels draw
their own. The frame is drawn in the terminal's *default* colour, which tmux sets per
pane (window-style / window-active-style, see layout.tag): blue on the active pane, grey
on the rest. Everything else a panel writes has an explicit colour (`use_palette`).
Not product code."""
import curses
import os

HEX = {"text": "#d8dee9", "bright": "#eceff4", "dim": "#8c9ab3", "line": "#4c566a", "surface": "#3b4252",
       "sel": "#5e81ac", "dark": "#2e3440", "blue": "#88c0d0", "cyan": "#8fbcbb", "green": "#a3be8c",
       "yellow": "#ebcb8b", "orange": "#d08770", "red": "#d57780", "magenta": "#b48ead"}
BASIC = {"text": curses.COLOR_WHITE, "bright": curses.COLOR_WHITE, "dim": curses.COLOR_WHITE, "line": curses.COLOR_WHITE,
         "surface": curses.COLOR_BLACK, "sel": curses.COLOR_BLUE, "dark": curses.COLOR_BLACK, "blue": curses.COLOR_BLUE,
         "cyan": curses.COLOR_CYAN, "green": curses.COLOR_GREEN, "yellow": curses.COLOR_YELLOW,
         "orange": curses.COLOR_YELLOW, "red": curses.COLOR_RED, "magenta": curses.COLOR_MAGENTA}


def xterm256(hexcolor):
    """The nearest xterm-256 colour (6x6x6 cube or grey ramp) to #rrggbb."""
    r, g, b = (int(hexcolor[i:i + 2], 16) for i in (1, 3, 5))
    steps = [0, 95, 135, 175, 215, 255]
    cube = [min(range(6), key=lambda i: abs(steps[i] - c)) for c in (r, g, b)]
    cube_rgb = [steps[i] for i in cube]
    grey = min(range(24), key=lambda i: abs(8 + 10 * i - (r + g + b) / 3))
    grey_rgb = [8 + 10 * grey] * 3
    dist = lambda rgb: sum((x - y) ** 2 for x, y in zip(rgb, (r, g, b)))  # noqa: E731
    return 232 + grey if dist(grey_rgb) < dist(cube_rgb) else 16 + 36 * cube[0] + 6 * cube[1] + cube[2]


_pairs = {}


def wrapper(main):
    """curses.wrapper, in direct colour where it can be. Inside tmux, curses starts on the
    tmux-direct terminfo, so the palette's hex colours are exact; tmux maps them down itself
    for terminals without true colour. Without that terminfo entry, the panel stays on 256
    colours. TERM is put back once curses has started: children (the agent, its shell
    commands) keep the pane's TERM, as tmux-direct is missing from the system terminfo."""
    term = os.environ.get("TERM", "")
    if os.environ.get("TMUX") and term.startswith("tmux") and term != "tmux-direct":
        try:
            curses.setupterm("tmux-direct")
        except curses.error:
            pass
        else:
            os.environ["TERM"] = "tmux-direct"

    def started(scr):
        os.environ["TERM"] = "tmux-256color" if term == "tmux-direct" else term  # tmux-direct: left over from an older run
        return main(scr)
    return curses.wrapper(started)


def color(fg, bg=None):
    """A colour-pair attribute for palette names (bg None = the terminal's background)."""
    key = (fg, bg)
    if key not in _pairs:
        f, b = (-1 if name in (None, "default") else _number(name) for name in (fg, bg))
        n = len(_pairs) + 1
        curses.init_pair(n, f, b)
        _pairs[key] = curses.color_pair(n)
    return _pairs[key]


def _number(name):
    if curses.COLORS >= 1 << 24:  # direct colour: the colour number is 0xRRGGBB
        return int(HEX[name][1:], 16)
    return xterm256(HEX[name]) if curses.COLORS >= 256 else BASIC[name]


def use_palette(win):
    """Give text written without a colour the palette's text colour, so only the frame
    (drawn in the default colour) follows tmux's active/inactive pane style."""
    win.bkgd(" ", color("text"))


def frame(win, title, right=()):
    """A rounded box over the whole window, with the title on the top edge (left) and
    optional (text, attr) segments on it (right). tmux colours it by pane focus."""
    h, w = win.getmaxyx()
    if h < 2 or w < 4:
        return
    edge = color("default")
    put = _putter(win, h, w)
    put(0, 0, "╭" + "─" * (w - 2) + "╮", edge)
    for y in range(1, h - 1):
        put(y, 0, "│", edge)
        put(y, w - 1, "│", edge)
    put(h - 1, 0, "╰" + "─" * (w - 2) + "╯", edge)
    x = 2
    if title:
        put(0, x, f" {title} ", edge | curses.A_BOLD)
        x += len(title) + 3
    right = list(right)
    while right and w - 3 - sum(len(t) for t, _ in right) <= x:  # too narrow: drop from the left
        right.pop(0)
    rx = w - 3 - sum(len(t) for t, _ in right)
    if right:
        for text, attr in right:
            put(0, rx, text, attr)
            rx += len(text)


def _putter(win, h, w):
    def put(y, x, text, attr):
        if 0 <= y < h and 0 <= x < w:
            try:
                win.addnstr(y, x, text, w - x, attr)
            except curses.error:  # writing the bottom-right cell moves the cursor off the window
                pass
    return put
