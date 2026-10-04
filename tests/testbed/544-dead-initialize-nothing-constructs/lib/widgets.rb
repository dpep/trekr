class Orphan
  def initialize
    @a = 1
  end
end

class Failure < StandardError
  def initialize(message = "failed")
    super
  end
end

class Built
  def initialize(one)
    @one = one
  end
end

class Made
  def initialize
    @b = 1
  end
end

class Abstract
  def initialize
    @c = 1
  end
end

class Concrete < Abstract
  def initialize
    super
  end
end

def build(klass)
  klass.new(1)
end

Made.new
Concrete.new
