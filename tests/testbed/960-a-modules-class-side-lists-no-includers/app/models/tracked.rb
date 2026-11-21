module Tracked
  extend ActiveSupport::Concern

  included do
    self.tracked_kinds = subclasses.map(&:name)
  end
end
