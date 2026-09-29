class Engine
  def start
  end
end

class Guarded
  def self.new(flag)
    return super() if flag
    Engine.new
  end

  def guarded_only
  end
end

class Factory
  def self.new(*args)
    Engine.new
  end
end

class Reset < Factory
  def self.new(*args)
    super
  end
end

guarded = Guarded.new(true)
guarded.guarded_only
guarded.start
reset = Reset.new
reset.start
