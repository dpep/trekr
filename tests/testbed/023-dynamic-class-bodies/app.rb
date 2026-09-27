class BaseThing
  def hello
    "hi"
  end
end

Sub = Class.new(BaseThing) do
  def extra
    hello
  end

  def hello
    super + "!"
  end
end

Failure = Class.new(StandardError)

Point = Struct.new(:x, :y) do
  def sum
    x + y
  end
end

Coord = Data.define(:lat, :lng) do
  def pair
    [lat, lng]
  end
end

Helpers = Module.new do
  def helper
    "help"
  end
end

class User
  def run
    sub = Sub.new
    sub.extra
    point = Point.new(1, 2)
    point.x = 3
    point.sum
    Coord.new(lat: 1, lng: 2).pair
  end
end

class Helped
  include Helpers

  def go
    helper
  end
end
