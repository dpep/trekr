module Pretend
  A = AuditConcern
  extend A
  module ClassMethods
    def pretend_fields; 1; end
  end
end
