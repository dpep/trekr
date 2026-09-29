module PathHelpers
  def root(path)
    Pathname(path)
  end

  def count(value)
    Integer(value) + Array(value).size + String(value).size
  end

  def zz_missing
    no_such_helper
  end
end

module Kernel
  def Pathname(path)
    path
  end
end

class Widget
  def to_s
    "widget"
  end
end

module Shown
  def label
    to_s
  end
end

class Host
  include Shown

  def to_s
    "host"
  end
end
