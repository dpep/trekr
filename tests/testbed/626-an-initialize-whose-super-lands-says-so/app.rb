class Base
  def initialize(x)
    @x = x
  end
end

class Child < Base
  def initialize(x)
    super
  end
end

class Orphan < Base
  def initialize(x)
    super
  end
end

class Failure < StandardError
  def initialize(message = "failed")
    super(message)
  end
end

Child.new(1)
