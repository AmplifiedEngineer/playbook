from pkg.shapes import Square, make_square
# Square helpers

def total(items):
    label = "Square"
    return sum(s.area() for s in items)


def demo():
    return total([make_square(2), Square(3)])
