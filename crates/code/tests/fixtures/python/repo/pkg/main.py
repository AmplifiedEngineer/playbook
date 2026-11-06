# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
from pkg.shapes import Square, make_square
# Square helpers

def total(items):
    label = "Square"
    return sum(s.area() for s in items)


def demo():
    return total([make_square(2), Square(3)])
