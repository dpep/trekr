module Tracking
  def self.included(base)
    base.class_eval <<~RUBY
      def track_it = send(:track_change)
    RUBY
  end

  def track_change = 1
end

class Tracked
  include Tracking
end
