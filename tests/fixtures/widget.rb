# A generic fixture exercising the fact set the blob layer promises.
module Registry
  DEFAULT = :basic
  ALIAS = DEFAULT

  def self.lookup(key)
    HANDLERS.fetch(key)
  end
end

module Trackable
  def track(event, **meta)
    Registry.lookup(event)
  end
end

class Widget < Base::Component
  include Trackable
  prepend Auditing
  extend Registry

  attr_reader :name
  attr_accessor :size
  attr_writer :label

  sig { returns(String) }
  def title
    @name.upcase
  end

  def resize(width, height = 1, *rest, depth:, unit: :cm, **opts, &blk)
    helper
    self.size = width
    box = Registry.lookup(unit)
    box.compute(width, height)
  end

  private

  def helper
    Registry::DEFAULT
  end

  def another
  end

  public :another

  class << self
    def build(*args)
      new(*args)
    end
  end
end

module Util
  module_function

  def normalize(text)
    text.strip
  end
end

class Widget
  alias_method :label, :name
  alias caption title
end
