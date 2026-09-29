module Connecting
  def connect
  end
end

class Base
  extend Connecting

  def save
  end

  def run
    save
  end
end

class Child < Base
  def touch
    save
  end
end

class Grandchild < Child
end

class Sibling < Base
end

class Override < Child
  def save
    super
  end
end

class Runner
  extend T::Sig

  sig { params(record: Base).void }
  def self.persist(record)
    record.save
  end

  def self.named(grandchild)
    grandchild.save
  end

  def self.made
    Child.new.save
    Grandchild.new.save
    Base.new.save
    Sibling.new.save
    Override.new.save
  end

  def self.connected
    Child.connect
    Grandchild.connect
    Base.connect
    Sibling.connect
  end
end
