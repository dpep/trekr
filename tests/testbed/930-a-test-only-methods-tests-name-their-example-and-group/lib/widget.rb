class Widget
  def initialize(size)
    @size = size
  end

  def size
    @size
  end

  def total
    @size * 2
  end

  def label
    "w#{@size}"
  end

  def mixed
    @size + 1
  end

  def peek
    @size
  end

  def tally
    @size
  end

  def shade
    @size
  end

  def self.build
    new(1)
  end
end

class Gadget
  def spin
    1
  end
end

class Gizmo
  def whirl
    2
  end
end

class Runner
  def run
    Widget.new(2).size
  end
end

Runner.new.run
