"""THROWAWAY PROTOTYPE: the shared look of moa's panels. One palette (Tokyo Night, the
same hex colours as tmux.conf, mapped to the 256-colour cube) and a rounded frame with
the title on its top edge. tmux can't round its own pane borders, so moa's panels draw
their own. The frame is drawn in the terminal's *default* colour, which tmux sets per
pane (window-style / window-active-style, see layout.tag): blue on the active pane, grey
on the rest. Everything else a panel writes has an explicit colour (`use_palette`).
Not product code."""
import curses

HEX = {"text": "#a9b1d6", "bright": "#c0caf5", "dim": "#565f89", "line": "#3b4261", "surface": "#292e42",
       "sel": "#364a82", "dark": "#1a1b26", "blue": "#7aa2f7", "cyan": "#7dcfff", "green": "#9ece6a",
       "yellow": "#e0af68", "orange": "#ff9e64", "red": "#f7768e", "magenta": "#bb9af7"}
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


def color(fg, bg=None):
    """A colour-pair attribute for palette names (bg None = the terminal's background)."""
    key = (fg, bg)
    if key not in _pairs:
        rich = curses.COLORS >= 256
        f = -1 if fg == "default" else xterm256(HEX[fg]) if rich else BASIC[fg]
        b = -1 if bg is None else xterm256(HEX[bg]) if rich else BASIC[bg]
        n = len(_pairs) + 1
        curses.init_pair(n, f, b)
        _pairs[key] = curses.color_pair(n)
    return _pairs[key]


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
