module Trackable
  extend AuditConcern

  module ClassMethods
    def tracked_fields
      [:id]
    end
  end

  def self.track!(x)
    audit_log(x)
  end
end
