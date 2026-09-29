module Auditing
  def label
    "audited"
  end

  def trail
  end
end

module Tracked
  include Auditing
end

class Base
  def label
    "base"
  end
end

class Child < Base
  include Auditing
end

class Grandchild < Child
  def label
    "grandchild"
  end
end

class Other
  include Tracked
end

class Runner
  extend T::Sig

  sig { params(item: Base).void }
  def self.run(item)
    item.label
    item.trail
  end

  def self.made
    Base.new.label
    Other.new.label
  end
end
