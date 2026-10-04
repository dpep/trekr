class Base
  def initialize(name)
    @name = name
  end

  def self.build(name)
    new(name)
  end
end

class Plain < Base
end

class Own < Base
  def initialize(name, size)
    super(name)
    @size = size
  end
end

Plain.new("a")
Own.new("a", 2)
Plain.public_send(:new, "b")

def make(klass)
  klass.new(1, 2)
end
