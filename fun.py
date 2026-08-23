#!/usr/bin/env python3
"""A fun little terminal toy: Matrix rain + a surprise at the end."""

import random
import shutil
import sys
import time

try:
    import colorama
    colorama.init()
    RED = "\033[91m"
    GREEN = "\033[92m"
    YELLOW = "\033[93m"
    CYAN = "\033[96m"
    RESET = "\033[0m"
except ImportError:
    RED = GREEN = YELLOW = CYAN = RESET = ""

# ASCII-safe characters so it works on any terminal (Windows cp1252 included)
CHARS = "01abcdefghijklmnopqrstuvwxyz0123456789#$%&*+=?/<>"

def matrix_rain(seconds=8):
    cols, rows = shutil.get_terminal_size((80, 24))
    # Each column has a "head" y position and a speed
    heads = [random.randint(-rows, 0) for _ in range(cols)]
    speeds = [random.randint(1, 4) for _ in range(cols)]

    start = time.time()
    while time.time() - start < seconds:
        frame = [[" " for _ in range(cols)] for _ in range(rows)]

        for c in range(cols):
            head = heads[c]
            speed = speeds[c]
            # Draw the trail behind the head
            for trail in range(3):
                y = head - trail
                if 0 <= y < rows:
                    ch = random.choice(CHARS)
                    if trail == 0:
                        frame[y][c] = f"{GREEN}{ch}{RESET}"
                    else:
                        frame[y][c] = f"{CYAN}{ch}{RESET}"
            heads[c] = head + speed
            if heads[c] > rows + 3:
                heads[c] = random.randint(-rows, -1)
                speeds[c] = random.randint(1, 4)

        sys.stdout.write("\033[H")  # move cursor home
        for row in frame:
            sys.stdout.write("".join(row) + "\n")
        sys.stdout.flush()
        time.sleep(0.05)

def surprise():
    """A little fortune cookie at the end."""
    fortunes = [
        "You are the 1% of the 1%.",
        "The matrix has you... but you have Python.",
        "42. That's it. That's the answer.",
        "Your code compiles on the first try. (Lies.)",
        "Somewhere, a rubber duck is proud of you.",
        "Real programmers count from 0.",
        "It's not a bug, it's an undocumented feature.",
        "May your coffee be strong and your bugs be weak.",
    ]
    print(f"\n{YELLOW}Fortune: {RESET}{random.choice(fortunes)}")
    print(f"{RED}Press Ctrl+C to escape the matrix... or don't.{RESET}\n")

if __name__ == "__main__":
    try:
        print(f"{GREEN}Entering the matrix...{RESET}")
        time.sleep(1)
        matrix_rain(seconds=8)
        surprise()
    except KeyboardInterrupt:
        print(f"\n{RED}You escaped the matrix. Impressive.{RESET}")
