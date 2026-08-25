module Auditable
  extend ActiveSupport::Concern

  class_methods do
    def audit_log
    end
  end

  def record
    self.class.audit_log
  end
end
