module ActiveSupport
  module Concern
    def included(base = nil, &block); end
  end
end

module Trackable
  extend ActiveSupport::Concern

  included do
    stamp!
  end

  def track
    polish
  end
end

class Widget
  include Trackable

  def stamp!
  end
end

module Loose
  def go
    stamp!
  end
end

class Gadget
  def polish
  end
end
