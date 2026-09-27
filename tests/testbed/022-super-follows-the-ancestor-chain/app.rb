module Prepended
  def greet
    "prepended-" + super
  end
end

module Included
  def greet
    "included-" + super
  end
end

class Base
  def greet
    "base"
  end

  def self.build
    new
  end
end

class Widget < Base
  prepend Prepended
  include Included

  def greet
    "widget-" + super
  end

  def self.build
    super
  end
end

class Shown
  def show
    widget = Widget.new
    widget.greet
  end
end
