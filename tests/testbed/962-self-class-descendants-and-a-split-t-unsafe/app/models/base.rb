class Base
  def kinds
    self.class.descendants.map(&:name)
  end
end

class Other
  def self.kinds
    T.unsafe(self)
      .subclasses
  end
end

class Third
  def self.kinds
    self.class.subclasses
  end
end
