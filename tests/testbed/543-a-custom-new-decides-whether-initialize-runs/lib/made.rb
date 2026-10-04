class Wrapped
  def self.new(*args)
    super
  end

  def initialize(a)
    @a = a
  end
end

class Cached
  def self.new(*args)
    @instance ||= allocate
  end

  def initialize
    @b = 1
  end
end

class Facade
  def self.new
    Wrapped.new(1)
  end

  def initialize
    @c = 1
  end
end

class Gadget
  def initialize
    @d = 1
  end
end

Wrapped.new(1)
Cached.new
Facade.new
Gadget.new
