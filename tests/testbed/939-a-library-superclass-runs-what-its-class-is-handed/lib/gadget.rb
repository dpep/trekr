require "ormkit"

class Notifier
  def self.ping
    raise "unreachable" if ENV["QUIET"]
  end
end

class Gadget < Ormkit::Base
  def label
    "gadget"
  end

  def after_build
    Notifier.ping
  end
end

class Plain
  def plain_label
    "plain"
  end
end
