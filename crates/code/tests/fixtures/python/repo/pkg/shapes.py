# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
SCALE = 2


class Square:
    def __init__(self, side):
        self.side = side

    def area(self):
        # Square area
        return self.side * self.side * SCALE


def make_square(side):
    return Square(side)
