module Factory
  extend self

  def new(options = {})
    Builder.new(options)
  end

  def [](name)
  end

  class Builder
    def [](name)
    end
  end
end

class Model
  def self.new(*args)
    super
  end

  def save
  end
end

class Opaque
  def self.new(*args)
    build(*args)
  end

  def save
  end
end

factory = Factory.new
factory[:x]
Model.new.save
opaque = Opaque.new
opaque.save
