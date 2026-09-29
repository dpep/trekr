module AutomaticDelegation
  def delegated
    :delegated
  end
end

module Extras
  def extra
    :extra
  end
end

class Decorator
  def self.delegate_all
    include AutomaticDelegation
  end

  def self.maybe_extras(on)
    include Extras if on
  end
end

class CommentDecorator < Decorator
  delegate_all
  maybe_extras true
end

class PlainDecorator < Decorator
end

module Tracked
  def tracked?
    true
  end
end

module TrackedFinders
  def find_tracked
    []
  end
end

module Macros
  def acts_as_tracked
    include Tracked
    extend TrackedFinders
  end
end

class Record
  extend Macros
end

class Item < Record
  acts_as_tracked
end
