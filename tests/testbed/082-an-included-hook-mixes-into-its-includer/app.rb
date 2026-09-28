module Tracking
  def self.included(base)
    base.extend(ClassMethods)
    base.include(Helpers)
    base.send(:include, Extras)
    base.extend(Optional) if base.respond_to?(:optional)
    base.class_eval do
      def tracked
      end

      def self.made
      end
    end
  end

  module ClassMethods
    def track
    end
  end

  module Helpers
    def help
    end
  end

  module Extras
    def extra
    end
  end

  module Optional
    def optional
    end
  end
end

class Widget
  include Tracking
end

Widget.track
Widget.new.help
Widget.new.extra
Widget.new.tracked
Widget.made
Widget.optional
Tracking.track

module Wrapping
  include Tracking
end

class Gadget
  include Wrapping
end

Gadget.new.help
Gadget.track

module Auditing
  class << self
    def extended(base)
      base.extend(Logging)
    end
  end

  module Logging
    def log
    end
  end
end

class Report
  extend Auditing
end

Report.log
