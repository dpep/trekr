module Disabling
  def run
    super
  end

  Widget.prepend self
end

module Naming
  def label
    "named"
  end

  Gadget.include(self)
end

module Remote
  def fetch
    :remote
  end

  Missing::Thing.send(:prepend, self)
end

module Outer
  module Inner
    Widget.include self
  end
end

module Helpful
  extend self

  def help
    :help
  end
end

module Orphan
  def lonely
  end
end
