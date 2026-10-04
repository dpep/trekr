module Store
  module Adapters
    class SchemaDumper < SchemaDumper
      private
        def extensions
          :adapter
        end
    end

    class SchemaCreation < SchemaCreation
    end
  end
end
