class Base
  def self.build(name)
    new
  end

  def self.fresh
    Base.new.build
  end

  def build
    :instance
  end
end

Base.build("z")

class Maker
  def make
    Base.build("y")
  end
end
