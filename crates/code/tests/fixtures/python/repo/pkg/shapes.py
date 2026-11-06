SCALE = 2


class Square:
    def __init__(self, side):
        self.side = side

    def area(self):
        # Square area
        return self.side * self.side * SCALE


def make_square(side):
    return Square(side)
