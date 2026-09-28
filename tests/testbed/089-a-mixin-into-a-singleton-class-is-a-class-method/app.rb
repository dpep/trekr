class Adapter
  def self.connect
  end

  def self.close
  end
end

module Retrying
  def connect
  end
end

module Closing
  def close
  end

  def shutdown
  end
end

Adapter.singleton_class.prepend(Retrying)
Adapter.singleton_class.include(Closing)

Adapter.connect
Adapter.close
Adapter.shutdown

module Pooling
  def pool
  end
end

class << Adapter
  include Pooling
end

Adapter.pool

module Naming
  def label
  end
end

class Widget
  class << self
    include Naming
  end

  singleton_class.prepend(Retrying)

  def self.connect
  end
end

Widget.label
Widget.connect
Widget.new.label
