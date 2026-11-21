class Base
end

class Registry
  def self.all
    T.unsafe(Base).subclasses + T.unsafe(registry).descendants
  end

  def self.registry
    Base
  end
end

class Leaf < Base
end

class Other
end

class OLeaf < Other
end
Registry.all
