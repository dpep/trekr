class Point < Struct.new(:x, :y)
  def norm
    x * x + y * y
  end
end

Pair = Struct.new(:left, :right) do
  def swap
    Pair.new(right, left)
  end
end

Coord = Data.define(:lat, :lng)

Opts = Struct.new(:size, keyword_init: true)

class Runner
  def go
    pt = Point.new(1, 2)
    pt.x
    pt.y = 3
    Pair.new(1, 2).swap
    Pair[1, 2]
    Coord.new(lat: 1, lng: 2).lat
    Opts.new(size: 1)
  end
end

Named = Struct.new(:name) do
  def initialize(name = "x")
    super
  end
end

Named.new
